//! Native Wayland drag source and clipboard selection.
//!
//! winit 0.30 exposes no seat, serial or data device, so we open our own
//! event queue on winit's `wl_display` (as Slint's clipboard does through
//! smithay-clipboard), bind our own `wl_seat`, `wl_pointer`, `wl_keyboard`
//! and `wl_data_device`, and remember the serial of the last button press
//! and key event. A drag is `wl_data_device.start_drag` on winit's surface
//! with that press serial; the compositor then owns the pointer until it
//! sends `dnd_finished` or `cancelled` on our source.

use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_data_device,
    wl_data_device::WlDataDevice, wl_data_offer::WlDataOffer, wl_data_source,
    wl_data_source::WlDataSource, wl_keyboard, wl_keyboard::WlKeyboard, wl_pointer,
    wl_pointer::WlPointer, wl_registry, wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{
    event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};

use super::{Icon, Payload, PORTAL};

pub(super) struct Wayland {
    connection: Connection,
    queue: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

type Done = Box<dyn FnOnce() + Send>;

/// One `wl_seat` and the objects we made on it. Every seat is bound: a drag
/// must start on the seat whose pointer delivered the press.
struct Seat {
    seat: WlSeat,
    device: Option<WlDataDevice>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
}

#[derive(Default)]
struct Shared {
    seats: Vec<Seat>,
    manager: Option<WlDataDeviceManager>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    /// Seat and serial of the last button press / key event.
    press: Option<(WlSeat, u32)>,
    key: Option<(WlSeat, u32)>,
    drag: Option<Drag>,
    selection: Option<WlDataSource>,
    offers: Vec<WlDataOffer>,
    portal: bool,
}

struct Drag {
    source: WlDataSource,
    icon: Option<IconSurface>,
    done: Option<Done>,
}

struct IconSurface {
    surface: WlSurface,
    buffer: WlBuffer,
    pool: WlShmPool,
}

impl IconSurface {
    fn destroy(self) {
        self.surface.destroy();
        self.buffer.destroy();
        self.pool.destroy();
    }
}

struct State {
    shared: Arc<Mutex<Shared>>,
}

/// What a data source serves; the portal key is made on first request.
struct Source {
    payload: Arc<Payload>,
    portal_key: Arc<Mutex<Option<Option<String>>>>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|error| error.into_inner())
}

impl Shared {
    /// The data device of the seat that produced `event` (a press or key).
    fn device_for(&self, event: &Option<(WlSeat, u32)>) -> Option<(WlDataDevice, u32)> {
        let (seat, serial) = event.as_ref()?;
        let device = self
            .seats
            .iter()
            .find(|entry| &entry.seat == seat)?
            .device
            .clone()?;
        Some((device, *serial))
    }
}

/// How long the dispatch thread waits on the socket before it rechecks
/// `stop`; it bounds how long dropping [`Wayland`] can take, whatever the
/// compositor does.
const POLL_MS: i32 = 100;

fn dispatch_until_stopped(mut events: EventQueue<State>, mut state: State, stop: &AtomicBool) {
    use std::os::fd::AsRawFd;
    while !stop.load(Ordering::Acquire) {
        if events.dispatch_pending(&mut state).is_err() || events.flush().is_err() {
            return;
        }
        let Some(guard) = events.prepare_read() else {
            continue;
        };
        let mut fd = libc::pollfd {
            fd: guard.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut fd, 1, POLL_MS) };
        if ready > 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_backend::client::WaylandError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        } else if ready < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return;
        }
        // A timeout drops the guard, which cancels the read.
    }
}

impl Wayland {
    /// # Safety
    /// `display` is winit's live `wl_display`; it must outlive this value.
    pub(super) unsafe fn new(display: *mut std::ffi::c_void) -> Option<Self> {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let events: EventQueue<State> = connection.new_event_queue();
        let queue = events.handle();
        connection.display().get_registry(&queue, ());
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("dragout-portal".into())
                .spawn(move || {
                    let available = super::portal_available();
                    lock(&shared).portal = available;
                })
                .ok()?;
        }
        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("dragout-wayland".into())
                .spawn(move || dispatch_until_stopped(events, State { shared }, &stop))
                .ok()?
        };
        Some(Self {
            connection,
            queue,
            shared,
            stop,
            thread: Some(thread),
        })
    }

    /// # Safety
    /// `surface` is the window's live `wl_surface` (winit's, on this display).
    pub(super) unsafe fn start_drag(
        &self,
        surface: *mut std::ffi::c_void,
        payload: Payload,
        icon: Option<Icon>,
        done: Done,
    ) -> bool {
        // Only a request argument: never dispatched or destroyed by us.
        let Some(origin) = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
            .ok()
            .and_then(|id| WlSurface::from_id(&self.connection, id).ok())
        else {
            return false;
        };
        let mut shared = lock(&self.shared);
        let (Some(manager), Some((device, serial))) =
            (shared.manager.clone(), shared.device_for(&shared.press))
        else {
            return false;
        };
        if let Some(old) = shared.drag.take() {
            end_drag(old);
        }
        let source = self.source(&manager, payload, &Payload::DRAG_TYPES, shared.portal);
        // Copy only: a drop must never move a download away from the app.
        if source.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        let icon = icon.and_then(|icon| {
            self.icon_surface(&shared, &icon)
                .map(|surface| (surface, icon))
        });
        device.start_drag(
            Some(&source),
            &origin,
            icon.as_ref().map(|(surface, _)| &surface.surface),
            serial,
        );
        if let Some((surface, icon)) = &icon {
            let scale = icon.scale.round().max(1.0);
            let integer = (icon.scale - scale).abs() < 0.01 && surface.surface.version() >= 3;
            if integer {
                surface.surface.set_buffer_scale(scale as i32);
            }
            // Below and right of the pointer, so the cursor does not hide it
            // (an attach offset; fine on the wl_surface v4 we bind).
            surface.surface.attach(Some(&surface.buffer), 12, 12);
            surface
                .surface
                .damage(0, 0, icon.width as i32, icon.height as i32);
            surface.surface.commit();
        }
        shared.drag = Some(Drag {
            source,
            icon: icon.map(|(surface, _)| surface),
            done: Some(done),
        });
        drop(shared);
        self.connection.flush().is_ok()
    }

    /// Ctrl+C: become the clipboard selection, serial from the key press.
    pub(super) fn set_selection(&self, payload: Payload) -> bool {
        let mut shared = lock(&self.shared);
        let (Some(manager), Some((device, serial))) =
            (shared.manager.clone(), shared.device_for(&shared.key))
        else {
            return false;
        };
        let source = self.source(&manager, payload, &Payload::COPY_TYPES, shared.portal);
        device.set_selection(Some(&source), serial);
        if let Some(old) = shared.selection.replace(source) {
            old.destroy();
        }
        drop(shared);
        self.connection.flush().is_ok()
    }

    fn source(
        &self,
        manager: &WlDataDeviceManager,
        payload: Payload,
        types: &[&str],
        portal: bool,
    ) -> WlDataSource {
        let source = manager.create_data_source(
            &self.queue,
            Source {
                payload: Arc::new(payload),
                portal_key: Arc::default(),
            },
        );
        for mime in types {
            source.offer((*mime).to_owned());
        }
        if portal {
            source.offer(PORTAL.to_owned());
        }
        source
    }

    fn icon_surface(&self, shared: &Shared, icon: &Icon) -> Option<IconSurface> {
        let (compositor, shm) = (shared.compositor.as_ref()?, shared.shm.as_ref()?);
        let stride = icon.width.checked_mul(4)?;
        let size = stride.checked_mul(icon.height)?;
        let file = shm_file(&argb(&icon.rgba))?;
        let pool = shm.create_pool(file.as_fd(), i32::try_from(size).ok()?, &self.queue, ());
        let buffer = pool.create_buffer(
            0,
            icon.width as i32,
            icon.height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &self.queue,
            (),
        );
        let surface = compositor.create_surface(&self.queue, ());
        Some(IconSurface {
            surface,
            buffer,
            pool,
        })
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        {
            let mut shared = lock(&self.shared);
            if let Some(drag) = shared.drag.take() {
                end_drag(drag);
            }
            if let Some(source) = shared.selection.take() {
                source.destroy();
            }
        }
        // The dispatch thread rechecks `stop` every POLL_MS without needing
        // the compositor, so this join is bounded; it must finish before
        // winit's display goes away.
        self.stop.store(true, Ordering::Release);
        let _ = self.connection.flush();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn end_drag(mut drag: Drag) {
    drag.source.destroy();
    if let Some(icon) = drag.icon.take() {
        icon.destroy();
    }
    if let Some(done) = drag.done.take() {
        done();
    }
}

/// Straight RGBA to `wl_shm` ARGB8888 (little-endian B, G, R, A, premultiplied).
fn argb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|pixel| {
            let alpha = u16::from(pixel[3]);
            let premultiply = |value: u8| (u16::from(value) * alpha / 255) as u8;
            [
                premultiply(pixel[2]),
                premultiply(pixel[1]),
                premultiply(pixel[0]),
                pixel[3],
            ]
        })
        .collect()
}

/// An unlinked 0600 file in the runtime directory holding `bytes`.
fn shm_file(bytes: &[u8]) -> Option<OwnedFd> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path = std::path::Path::new(&dir).join(format!(
        "minidl-drag-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    let _ = std::fs::remove_file(&path);
    file.write_all(bytes).ok()?;
    Some(file.into())
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        let mut shared = lock(&state.shared);
        match interface.as_str() {
            "wl_seat" => {
                let seat = registry.bind(name, version.min(5), queue, ());
                shared.seats.push(Seat {
                    seat,
                    device: None,
                    pointer: None,
                    keyboard: None,
                });
            }
            "wl_data_device_manager" if shared.manager.is_none() => {
                shared.manager = Some(registry.bind(name, version.min(3), queue, ()));
            }
            "wl_compositor" if shared.compositor.is_none() => {
                shared.compositor = Some(registry.bind(name, version.min(4), queue, ()));
            }
            "wl_shm" if shared.shm.is_none() => {
                shared.shm = Some(registry.bind(name, 1, queue, ()));
            }
            _ => return,
        }
        if let Some(manager) = shared.manager.clone() {
            for entry in shared
                .seats
                .iter_mut()
                .filter(|entry| entry.device.is_none())
            {
                entry.device = Some(manager.get_data_device(&entry.seat, queue, ()));
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        let mut shared = lock(&state.shared);
        let Some(entry) = shared.seats.iter_mut().find(|entry| &entry.seat == seat) else {
            return;
        };
        if capabilities.contains(wl_seat::Capability::Pointer) && entry.pointer.is_none() {
            entry.pointer = Some(seat.get_pointer(queue, seat.clone()));
        }
        if capabilities.contains(wl_seat::Capability::Keyboard) && entry.keyboard.is_none() {
            entry.keyboard = Some(seat.get_keyboard(queue, seat.clone()));
        }
    }
}

impl Dispatch<WlPointer, WlSeat> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        seat: &WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Every wl_pointer of the focused client gets the same button event,
        // so this is the serial of winit's implicit grab.
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            lock(&state.shared).press = Some((seat.clone(), serial));
        }
    }
}

impl Dispatch<WlKeyboard, WlSeat> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        seat: &WlSeat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } => {
                lock(&state.shared).key = Some((seat.clone(), serial));
            }
            wl_keyboard::Event::Leave { .. } => {
                let mut shared = lock(&state.shared);
                if shared.key.as_ref().is_some_and(|(owner, _)| owner == seat) {
                    shared.key = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // We never read offers (we are no drop target, and Slint's clipboard
        // has its own device): free each one once the next replaces it.
        let mut shared = lock(&state.shared);
        match event {
            wl_data_device::Event::DataOffer { id } => shared.offers.push(id),
            wl_data_device::Event::Selection { id } | wl_data_device::Event::Enter { id, .. } => {
                let keep = id.map(|offer| offer.id());
                shared.offers.retain(|offer| {
                    let current = Some(offer.id()) == keep;
                    if !current {
                        offer.destroy();
                    }
                    current
                });
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, Source> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        data: &Source,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                // Off the dispatch thread: a slow reader or the portal's
                // D-Bus calls must not stall pointer and key serials.
                let payload = data.payload.clone();
                let portal_key = data.portal_key.clone();
                let _ = std::thread::Builder::new()
                    .name("dragout-send".into())
                    .spawn(move || {
                        let bytes = if mime_type == PORTAL {
                            let mut key =
                                portal_key.lock().unwrap_or_else(|error| error.into_inner());
                            key.get_or_insert_with(|| super::portal_key(&payload.paths))
                                .clone()
                                .map(String::into_bytes)
                        } else {
                            payload.data(&mime_type)
                        };
                        if let Some(bytes) = bytes {
                            let _ = std::fs::File::from(fd).write_all(&bytes);
                        }
                    });
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                let mut shared = lock(&state.shared);
                if shared
                    .drag
                    .as_ref()
                    .is_some_and(|drag| &drag.source == source)
                {
                    let drag = shared.drag.take().expect("checked above");
                    drop(shared);
                    end_drag(drag);
                } else if shared.selection.as_ref() == Some(source) {
                    shared.selection = None;
                    source.destroy();
                }
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),*) => {$(
        impl Dispatch<$interface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$interface,
                _: <$interface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    WlDataDeviceManager,
    WlDataOffer,
    WlCompositor,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlCallback
);

#[cfg(test)]
mod tests {
    #[test]
    fn icon_pixels_become_premultiplied_little_endian_argb() {
        assert_eq!(
            super::argb(&[255, 128, 0, 255, 200, 100, 50, 0, 255, 255, 255, 128]),
            [0, 128, 255, 255, 0, 0, 0, 0, 128, 128, 128, 128]
        );
    }
}
