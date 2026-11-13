//! XDND (version 5) drag source for X11 and XWayland.
//!
//! winit swallows the events a source needs (XdndStatus, XdndFinished,
//! SelectionRequest), so the drag runs on a second connection and thread:
//! an unmapped window owns `XdndSelection`, winit's implicit pointer grab
//! from the press is released through its own Xlib display, and we grab the
//! pointer and keyboard until the drop, a refusal or Escape.
//! `Session` is the protocol state machine, free of I/O.

use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
    GrabStatus, KeyButMask, PropMode, SelectionNotifyEvent, SelectionRequestEvent, Window,
    WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

use super::{Payload, TEXT, TEXT_UTF8, URI_LIST};

/// The XDND version we speak; targets below 3 are not supported.
const VERSION: u32 = 5;
const ESCAPE: u32 = 0xff1b;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        XdndAware,
        XdndProxy,
        XdndTypeList,
        XdndSelection,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndActionCopy,
        TARGETS,
        UTF8_STRING,
        URI_LIST: b"text/uri-list",
        TEXT_UTF8: b"text/plain;charset=utf-8",
        TEXT: b"text/plain",
        MINIDL_DRAG_TIME,
    }
}

/// A drop target: the XdndAware `window`, reached through `proxy` (itself
/// unless it set XdndProxy), speaking `version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) window: u32,
    pub(super) proxy: u32,
    pub(super) version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Msg {
    Enter,
    Position { x: i16, y: i16, time: u32 },
    Leave,
    Drop { time: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Dragging,
    /// Released while a position was unanswered: decide on its status.
    Releasing {
        time: u32,
    },
    Dropped(Target),
    Over,
}

/// The source side of XDND: what to send for each pointer and target event.
#[derive(Debug)]
pub(super) struct Session {
    target: Option<Target>,
    waiting: bool,
    queued: Option<(i16, i16, u32)>,
    accepted: bool,
    phase: Phase,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            target: None,
            waiting: false,
            queued: None,
            accepted: false,
            phase: Phase::Dragging,
        }
    }
}

impl Session {
    pub(super) fn motion(
        &mut self,
        target: Option<Target>,
        x: i16,
        y: i16,
        time: u32,
    ) -> Vec<(Target, Msg)> {
        let mut out = Vec::new();
        if self.phase != Phase::Dragging {
            return out;
        }
        if self.target != target {
            if let Some(old) = self.target.take() {
                out.push((old, Msg::Leave));
            }
            self.waiting = false;
            self.queued = None;
            self.accepted = false;
            if let Some(new) = target {
                out.push((new, Msg::Enter));
                self.target = Some(new);
            }
        }
        if let Some(target) = self.target {
            // One unanswered XdndPosition at a time; keep only the latest.
            if self.waiting {
                self.queued = Some((x, y, time));
            } else {
                self.waiting = true;
                out.push((target, Msg::Position { x, y, time }));
            }
        }
        out
    }

    pub(super) fn status(&mut self, from: u32, accept: bool) -> Vec<(Target, Msg)> {
        let Some(target) = self.target.filter(|target| target.window == from) else {
            return Vec::new();
        };
        self.accepted = accept;
        self.waiting = false;
        match self.phase {
            Phase::Releasing { time } => self.decide(time),
            Phase::Dragging => match self.queued.take() {
                Some((x, y, time)) => {
                    self.waiting = true;
                    vec![(target, Msg::Position { x, y, time })]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    pub(super) fn release(&mut self, time: u32) -> Vec<(Target, Msg)> {
        if self.phase != Phase::Dragging {
            return Vec::new();
        }
        if self.target.is_some() && self.waiting {
            self.phase = Phase::Releasing { time };
            return Vec::new();
        }
        self.decide(time)
    }

    fn decide(&mut self, time: u32) -> Vec<(Target, Msg)> {
        match self.target.take() {
            Some(target) if self.accepted => {
                self.phase = Phase::Dropped(target);
                vec![(target, Msg::Drop { time })]
            }
            Some(target) => {
                self.phase = Phase::Over;
                vec![(target, Msg::Leave)]
            }
            None => {
                self.phase = Phase::Over;
                Vec::new()
            }
        }
    }

    /// Escape, a target that never answers, or a lost grab.
    pub(super) fn cancel(&mut self) -> Vec<(Target, Msg)> {
        self.phase = Phase::Over;
        self.target
            .take()
            .map(|target| (target, Msg::Leave))
            .into_iter()
            .collect()
    }

    pub(super) fn finished(&mut self, from: u32) {
        if matches!(self.phase, Phase::Dropped(target) if target.window == from) {
            self.phase = Phase::Over;
        }
    }

    pub(super) fn accepted(&self) -> bool {
        self.accepted && self.target.is_some()
    }

    pub(super) fn released(&self) -> bool {
        self.phase != Phase::Dragging
    }

    pub(super) fn over(&self) -> bool {
        self.phase == Phase::Over
    }
}

/// How long after the release (or the last data request) to wait before
/// giving up: a target that never answers the release is dropped after 1 s;
/// one that accepted the drop gets 30 s to send XdndFinished (it may show a
/// dialog first), and our window keeps serving its data until then.
fn give_up_after(phase: Phase) -> Duration {
    match phase {
        Phase::Dropped(_) => Duration::from_secs(30),
        _ => Duration::from_secs(1),
    }
}

/// The five data words of a source message (`types`: our first three).
pub(super) fn message_data(
    msg: Msg,
    source: u32,
    target: Target,
    types: &[u32],
    copy: u32,
) -> [u32; 5] {
    match msg {
        Msg::Enter => {
            let more = u32::from(types.len() > 3);
            let first = |index: usize| types.get(index).copied().unwrap_or(NONE);
            [
                source,
                target.version.min(VERSION) << 24 | more,
                first(0),
                first(1),
                first(2),
            ]
        }
        Msg::Position { x, y, time } => [
            source,
            0,
            u32::from(x as u16) << 16 | u32::from(y as u16),
            time,
            copy,
        ],
        Msg::Leave => [source, 0, 0, 0, 0],
        Msg::Drop { time } => [source, 0, time, 0, 0],
    }
}

/// XdndStatus: (target window, accepted).
pub(super) fn parse_status(data: [u32; 5]) -> (u32, bool) {
    (data[0], data[1] & 1 == 1)
}

/// A usable XdndAware value: version 3 or later.
pub(super) fn usable_version(aware: Option<u32>) -> Option<u32> {
    aware.filter(|version| *version >= 3)
}

type Done = Box<dyn FnOnce() + Send>;

/// Hand winit's pointer grab over and run the drag on a thread; `done`
/// runs when it is over. False if there is no usable X connection.
pub(super) fn start_drag(
    xlib_display: usize,
    own_window: u32,
    payload: Payload,
    done: Done,
) -> bool {
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else {
        return false;
    };
    let Some(mut drag) = XDrag::new(own_window, payload) else {
        return false;
    };
    let display = xlib_display as *mut x11_dl::xlib::Display;
    // Safety: winit's live display, used on its own (the UI) thread.
    // shortcut: the pointer is ungrabbed for the instant until our grab
    // lands; a release in that gap ends the drag (checked after grabbing).
    unsafe {
        (xlib.XUngrabPointer)(display, x11_dl::xlib::CurrentTime);
        (xlib.XFlush)(display);
    }
    std::thread::Builder::new()
        .name("dragout-x11".into())
        .spawn(move || {
            drag.run();
            done();
        })
        .is_ok()
}

struct XDrag {
    conn: RustConnection,
    root: Window,
    window: Window,
    own_window: Window,
    atoms: Atoms,
    types: Vec<(Atom, &'static str)>,
    payload: Payload,
    escape: Vec<u8>,
    cursors: (u32, u32),
    time: u32,
}

impl XDrag {
    fn new(own_window: Window, payload: Payload) -> Option<Self> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        let window = conn.generate_id().ok()?;
        conn.create_window(
            0,
            window,
            root,
            -10,
            -10,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .ok()?;
        let types = vec![
            (atoms.URI_LIST, URI_LIST),
            (atoms.TEXT_UTF8, TEXT_UTF8),
            (atoms.TEXT, TEXT),
            (atoms.UTF8_STRING, "UTF8_STRING"),
        ];
        let listed = types
            .iter()
            .map(|(atom, _)| *atom)
            .take(3)
            .collect::<Vec<_>>();
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms.XdndTypeList,
            AtomEnum::ATOM,
            &listed,
        )
        .ok()?;
        let cursors = x11rb::resource_manager::new_from_default(&conn)
            .ok()
            .and_then(|database| {
                x11rb::cursor::Handle::new(&conn, screen, &database)
                    .ok()?
                    .reply()
                    .ok()
            })
            .map(|handle| {
                let load = |name| handle.load_cursor(&conn, name).unwrap_or(NONE);
                (load("dnd-copy"), load("dnd-no-drop"))
            })
            .unwrap_or((NONE, NONE));
        let setup = conn.setup();
        let mapping = conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
            .ok()?
            .reply()
            .ok()?;
        let per = usize::from(mapping.keysyms_per_keycode.max(1));
        let escape = mapping
            .keysyms
            .chunks(per)
            .enumerate()
            .filter(|(_, syms)| syms.contains(&ESCAPE))
            .map(|(index, _)| setup.min_keycode + index as u8)
            .collect();
        let mut drag = Self {
            conn,
            root,
            window,
            own_window,
            atoms,
            types,
            payload,
            escape,
            cursors,
            time: CURRENT_TIME,
        };
        drag.time = drag.server_time()?;
        drag.conn
            .set_selection_owner(window, drag.atoms.XdndSelection, drag.time)
            .ok()?;
        drag.conn.flush().ok()?;
        Some(drag)
    }

    /// ICCCM: a real timestamp from a zero-length property append.
    fn server_time(&self) -> Option<u32> {
        let atom = self.atoms.MINIDL_DRAG_TIME;
        self.conn
            .change_property8(PropMode::APPEND, self.window, atom, AtomEnum::STRING, &[])
            .ok()?;
        self.conn.flush().ok()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.conn.poll_for_event().ok()? {
                Some(Event::PropertyNotify(event)) if event.window == self.window => {
                    return Some(event.time);
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        None
    }

    fn run(&mut self) {
        if self.grab() {
            self.drive();
        }
        let _ = self.conn.ungrab_pointer(CURRENT_TIME);
        let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }

    fn grab(&self) -> bool {
        let mask = EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
        let mut grabbed = false;
        for _ in 0..40 {
            let status = self
                .conn
                .grab_pointer(
                    false,
                    self.root,
                    mask,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    NONE,
                    self.cursors.1,
                    CURRENT_TIME,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.status);
            if status == Some(GrabStatus::SUCCESS) {
                grabbed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !grabbed {
            return false;
        }
        let _ = self.conn.grab_keyboard(
            false,
            self.root,
            CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        );
        // Released while the grab changed hands: no drag after all.
        self.conn
            .query_pointer(self.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.mask.contains(KeyButMask::BUTTON1))
    }

    fn drive(&mut self) {
        let mut session = Session::default();
        let mut accepted = false;
        let mut released_at = None;
        let mut handed_back = false;
        if let Ok(Ok(pointer)) = self
            .conn
            .query_pointer(self.root)
            .map(|cookie| cookie.reply())
        {
            let target = self.target_at(pointer.root_x, pointer.root_y);
            let out = session.motion(target, pointer.root_x, pointer.root_y, self.time);
            self.send(out);
        }
        while !session.over() {
            let event = match self.conn.poll_for_event() {
                Ok(Some(event)) => event,
                Ok(None) => {
                    let limit = give_up_after(session.phase);
                    if released_at.is_some_and(|at: Instant| at.elapsed() > limit) {
                        let out = session.cancel();
                        self.send(out);
                        break;
                    }
                    if self.conn.flush().is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(4));
                    continue;
                }
                Err(_) => break,
            };
            let out = match event {
                Event::MotionNotify(event) => {
                    let target = self.target_at(event.root_x, event.root_y);
                    session.motion(target, event.root_x, event.root_y, event.time)
                }
                Event::ButtonRelease(event) if event.detail == 1 => {
                    released_at = Some(Instant::now());
                    session.release(event.time)
                }
                Event::KeyPress(event) if self.escape.contains(&event.detail) => {
                    released_at = Some(Instant::now());
                    session.cancel()
                }
                Event::ClientMessage(event) if event.type_ == self.atoms.XdndStatus => {
                    let (from, accept) = parse_status(event.data.as_data32());
                    session.status(from, accept)
                }
                Event::ClientMessage(event) if event.type_ == self.atoms.XdndFinished => {
                    session.finished(event.data.as_data32()[0]);
                    Vec::new()
                }
                Event::SelectionRequest(request) => {
                    self.answer(&request);
                    // A target still reading the data keeps the selection
                    // owner (our window) alive.
                    if matches!(session.phase, Phase::Dropped(_)) {
                        released_at = Some(Instant::now());
                    }
                    Vec::new()
                }
                _ => Vec::new(),
            };
            self.send(out);
            if !session.released() && session.accepted() != accepted {
                accepted = session.accepted();
                let cursor = if accepted {
                    self.cursors.0
                } else {
                    self.cursors.1
                };
                let mask = EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
                let _ = self
                    .conn
                    .change_active_pointer_grab(cursor, CURRENT_TIME, mask);
            }
            if session.released() && !handed_back {
                // The drop is decided: hand the pointer back right away.
                handed_back = true;
                let _ = self.conn.ungrab_pointer(CURRENT_TIME);
                let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
            }
        }
    }

    fn send(&self, messages: Vec<(Target, Msg)>) {
        let listed = self.types.iter().map(|(atom, _)| *atom).collect::<Vec<_>>();
        for (target, msg) in messages {
            let kind = match msg {
                Msg::Enter => self.atoms.XdndEnter,
                Msg::Position { .. } => self.atoms.XdndPosition,
                Msg::Leave => self.atoms.XdndLeave,
                Msg::Drop { .. } => self.atoms.XdndDrop,
            };
            let data = message_data(msg, self.window, target, &listed, self.atoms.XdndActionCopy);
            let event = ClientMessageEvent::new(32, target.window, kind, data);
            let _ = self
                .conn
                .send_event(false, target.proxy, EventMask::NO_EVENT, event);
        }
        let _ = self.conn.flush();
    }

    /// The topmost XdndAware window under the root position, minus our own.
    fn target_at(&self, x: i16, y: i16) -> Option<Target> {
        let mut window = self.root;
        for _ in 0..32 {
            if let Some(target) = self.aware(window) {
                return (target.window != self.own_window).then_some(target);
            }
            let child = self
                .conn
                .translate_coordinates(self.root, window, x, y)
                .ok()?
                .reply()
                .ok()?
                .child;
            if child == NONE {
                return None;
            }
            window = child;
        }
        None
    }

    fn aware(&self, window: Window) -> Option<Target> {
        let proxy = self
            .property(window, self.atoms.XdndProxy, AtomEnum::WINDOW.into())
            // A proxy must point to itself, or it is stale.
            .filter(|proxy| {
                self.property(*proxy, self.atoms.XdndProxy, AtomEnum::WINDOW.into()) == Some(*proxy)
            })
            .unwrap_or(window);
        let version =
            usable_version(self.property(proxy, self.atoms.XdndAware, AtomEnum::ATOM.into()))?;
        Some(Target {
            window,
            proxy,
            version,
        })
    }

    fn property(&self, window: Window, property: Atom, kind: Atom) -> Option<u32> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let value = reply.value32()?.next();
        value
    }

    fn answer(&self, request: &SelectionRequestEvent) {
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let served = request.selection == self.atoms.XdndSelection && self.serve(request, property);
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { NONE },
        };
        let _ = self
            .conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
    }

    fn serve(&self, request: &SelectionRequestEvent, property: Atom) -> bool {
        if request.target == self.atoms.TARGETS {
            let mut targets = vec![self.atoms.TARGETS];
            targets.extend(self.types.iter().map(|(atom, _)| *atom));
            return self
                .conn
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )
                .is_ok();
        }
        let Some(bytes) = self
            .types
            .iter()
            .find(|(atom, _)| *atom == request.target)
            .and_then(|(_, mime)| self.payload.data(mime))
        else {
            return false;
        };
        // shortcut: no INCR transfers; a list of file URIs is far below the
        // request limit (256 KiB, or 16 MiB with BIG-REQUESTS).
        if bytes.len() + 64 > self.conn.maximum_request_bytes() {
            return false;
        }
        self.conn
            .change_property8(
                PropMode::REPLACE,
                request.requestor,
                property,
                request.target,
                &bytes,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Target = Target {
        window: 10,
        proxy: 10,
        version: 5,
    };
    const B: Target = Target {
        window: 20,
        proxy: 21,
        version: 4,
    };

    fn pos(x: i16, y: i16, time: u32) -> Msg {
        Msg::Position { x, y, time }
    }

    #[test]
    fn entering_moving_and_leaving_targets() {
        let mut session = Session::default();
        assert!(
            session.motion(None, 1, 1, 1).is_empty(),
            "nothing under the pointer"
        );
        assert_eq!(
            session.motion(Some(A), 2, 2, 2),
            [(A, Msg::Enter), (A, pos(2, 2, 2))]
        );
        assert!(
            session.motion(Some(A), 3, 3, 3).is_empty(),
            "wait for XdndStatus"
        );
        assert!(session.motion(Some(A), 4, 4, 4).is_empty());
        assert_eq!(
            session.status(A.window, true),
            [(A, pos(4, 4, 4))],
            "only the latest goes out"
        );
        assert!(session.accepted());
        assert!(
            session.status(B.window, false).is_empty(),
            "a stranger's status is ignored"
        );
        assert_eq!(
            session.motion(Some(B), 5, 5, 5),
            [(A, Msg::Leave), (B, Msg::Enter), (B, pos(5, 5, 5))]
        );
        assert!(!session.accepted(), "acceptance belongs to the old target");
        assert_eq!(session.motion(None, 6, 6, 6), [(B, Msg::Leave)]);
    }

    #[test]
    fn an_accepted_drop_is_served_for_thirty_seconds() {
        assert_eq!(give_up_after(Phase::Dropped(A)), Duration::from_secs(30));
        assert_eq!(give_up_after(Phase::Dragging), Duration::from_secs(1));
    }

    #[test]
    fn a_release_drops_only_on_an_accepting_target_and_waits_for_finished() {
        let mut session = Session::default();
        session.motion(Some(A), 1, 1, 1);
        session.status(A.window, true);
        assert_eq!(session.release(9), [(A, Msg::Drop { time: 9 })]);
        assert!(
            session.motion(Some(B), 1, 1, 1).is_empty(),
            "no moves after the release"
        );
        assert!(!session.over());
        session.finished(B.window);
        assert!(!session.over(), "only the drop target finishes");
        session.finished(A.window);
        assert!(session.over());

        let mut refused = Session::default();
        refused.motion(Some(A), 1, 1, 1);
        refused.status(A.window, false);
        assert_eq!(refused.release(9), [(A, Msg::Leave)]);
        assert!(refused.over());

        let mut nowhere = Session::default();
        assert!(nowhere.release(9).is_empty());
        assert!(nowhere.over());
    }

    #[test]
    fn a_release_before_the_status_waits_for_it() {
        let mut session = Session::default();
        session.motion(Some(A), 1, 1, 1);
        assert!(session.release(7).is_empty());
        assert!(session.released() && !session.over());
        assert_eq!(session.status(A.window, true), [(A, Msg::Drop { time: 7 })]);

        let mut refused = Session::default();
        refused.motion(Some(A), 1, 1, 1);
        refused.release(7);
        assert_eq!(refused.status(A.window, false), [(A, Msg::Leave)]);
        assert!(refused.over());
    }

    #[test]
    fn escape_leaves_the_current_target() {
        let mut session = Session::default();
        session.motion(Some(B), 1, 1, 1);
        assert_eq!(session.cancel(), [(B, Msg::Leave)]);
        assert!(session.over());
        assert!(Session::default().cancel().is_empty());
    }

    #[test]
    fn messages_follow_the_xdnd_layout() {
        let types = [101, 102, 103, 104];
        assert_eq!(
            message_data(Msg::Enter, 7, A, &types, 50),
            [7, 5 << 24 | 1, 101, 102, 103]
        );
        assert_eq!(
            message_data(Msg::Enter, 7, B, &types[..2], 50),
            [7, 4 << 24, 101, 102, NONE]
        );
        let far = Target { version: 9, ..A };
        assert_eq!(
            message_data(Msg::Enter, 7, far, &types, 50)[1] >> 24,
            5,
            "never above ours"
        );
        assert_eq!(
            message_data(pos(300, 2, 99), 7, A, &types, 50),
            [7, 0, 300 << 16 | 2, 99, 50]
        );
        assert_eq!(
            message_data(pos(-1, 0, 1), 7, A, &types, 50)[2],
            0xffff_0000,
            "negative coordinates wrap like C"
        );
        assert_eq!(message_data(Msg::Leave, 7, A, &types, 50), [7, 0, 0, 0, 0]);
        assert_eq!(
            message_data(Msg::Drop { time: 5 }, 7, A, &types, 50),
            [7, 0, 5, 0, 0]
        );
        assert_eq!(parse_status([10, 3, 0, 0, 50]), (10, true));
        assert_eq!(parse_status([10, 2, 0, 0, 0]), (10, false));
        assert_eq!(usable_version(Some(5)), Some(5));
        assert_eq!(usable_version(Some(2)), None);
        assert_eq!(usable_version(None), None);
    }
}
