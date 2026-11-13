//! Drag finished downloads out of the window into any other application, as
//! if they came from a file manager.
//!
//! Slint's own drag and drop stays inside one window and winit has no drag
//! source, so each platform talks to the system itself: `wayland.rs`
//! (wl_data_device on winit's own display connection), `x11.rs` (an XDND
//! source on a second connection) and `windows.rs` (the shell's
//! `SHDoDragDrop`). The decisions (gesture, which rows, the URI list) live
//! here, free of I/O, and are unit-tested.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use minidl_app::controller::DownloadView;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(any(windows, test))]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

/// Logical pixels a pressed pointer must travel before a drag starts (the
/// GTK default; small enough to feel immediate, large enough for a click).
const THRESHOLD: f32 = 8.0;

pub(crate) const URI_LIST: &str = "text/uri-list";
pub(crate) const TEXT_UTF8: &str = "text/plain;charset=utf-8";
pub(crate) const TEXT: &str = "text/plain";
/// What GNOME's file manager pastes; drags never need it.
const GNOME_COPIED: &str = "x-special/gnome-copied-files";
/// The XDG document portal's key, for receivers inside a Flatpak or Snap.
#[cfg(target_os = "linux")]
const PORTAL: &str = "application/vnd.portal.filetransfer";

/// Press-and-move on one row: armed by a left press, fires once when the
/// pointer has moved far enough. The card only reports moves while pressed.
#[derive(Debug, Default)]
pub(crate) struct Gesture {
    armed: Option<i32>,
}

impl Gesture {
    pub(crate) fn press(&mut self, id: i32) {
        self.armed = Some(id);
    }

    pub(crate) fn moved(&mut self, id: i32, dx: f32, dy: f32) -> bool {
        let fire = self.armed == Some(id) && dx.hypot(dy) >= THRESHOLD;
        if fire {
            self.armed = None;
        }
        fire
    }
}

/// The files a drag that starts on `pressed` carries, in list order. The
/// pressed row must itself be a finished file that still exists; if it is
/// selected, every selected such row comes along, otherwise only it.
pub(crate) fn drag_paths(
    rows: &[DownloadView],
    selection: &HashSet<i64>,
    pressed: i64,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let path = |row: &DownloadView| {
        row.path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && exists(path))
    };
    let Some(own) = rows.iter().find(|row| row.id == pressed).and_then(path) else {
        return Vec::new();
    };
    if !selection.contains(&pressed) {
        return vec![own];
    }
    rows.iter()
        .filter(|row| selection.contains(&row.id))
        .filter_map(path)
        .collect()
}

/// `text/uri-list` (RFC 2483): one `file://` URI per line, CRLF-terminated,
/// every byte outside RFC 3986's unreserved set and `/` percent-encoded, so
/// spaces, `%`, `#`, `?`, newlines and non-ASCII names survive.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|path| file_uri(path) + "\r\n").collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn file_uri(path: &Path) -> String {
    use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
    const PATH: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'/');
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec();
    #[cfg(not(unix))]
    let bytes = path.to_string_lossy().replace('\\', "/").into_bytes();
    let prefix = if bytes.first() == Some(&b'/') {
        "file://"
    } else {
        "file:///"
    };
    format!("{prefix}{}", percent_encode(&bytes, PATH))
}

/// The data a drag or a copy offers, by MIME type.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct Payload {
    pub(crate) paths: Vec<PathBuf>,
    uris: String,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl Payload {
    pub(crate) fn new(paths: Vec<PathBuf>) -> Self {
        let uris = uri_list(&paths);
        Self { paths, uris }
    }

    /// The types a drag offers, best first.
    pub(crate) const DRAG_TYPES: [&'static str; 3] = [URI_LIST, TEXT_UTF8, TEXT];
    /// A copy also offers what GNOME Files pastes.
    pub(crate) const COPY_TYPES: [&'static str; 4] = [URI_LIST, GNOME_COPIED, TEXT_UTF8, TEXT];

    pub(crate) fn data(&self, mime: &str) -> Option<Vec<u8>> {
        match mime {
            URI_LIST => Some(self.uris.clone().into_bytes()),
            // One path per line: readable text for an editor or a chat box.
            TEXT_UTF8 | TEXT | "UTF8_STRING" => Some(
                self.paths
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes(),
            ),
            GNOME_COPIED => {
                Some(format!("copy\n{}", self.uris.trim_end().replace("\r\n", "\n")).into_bytes())
            }
            _ => None,
        }
    }
}

/// A file dropped on our own window shortly after we dragged it out is our
/// own drag coming back (winit is a drop target on Windows): ignore it.
#[derive(Debug, Default)]
pub(crate) struct OwnDrops {
    last: Option<(Vec<PathBuf>, Instant)>,
}

impl OwnDrops {
    const WINDOW: Duration = Duration::from_secs(3);

    pub(crate) fn dragged(&mut self, paths: &[PathBuf], now: Instant) {
        self.last = Some((paths.to_vec(), now));
    }

    pub(crate) fn is_own(&self, path: &Path, now: Instant) -> bool {
        self.last.as_ref().is_some_and(|(paths, at)| {
            now.saturating_duration_since(*at) < Self::WINDOW && paths.iter().any(|own| own == path)
        })
    }
}

/// A drag image: straight RGBA rows, `scale` physical pixels per logical one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Icon {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale: f32,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) use platform::*;

mod platform {
    use super::*;
    use crate::MainWindow;
    use slint::ComponentHandle;
    use std::cell::RefCell;

    use slint::winit_030::winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    use slint::winit_030::WinitWindowAccessor;

    enum Backend {
        /// Our own queue on winit's display, recording press and key serials.
        #[cfg(target_os = "linux")]
        Wayland(super::wayland::Wayland),
        /// X11 and Windows need nothing ahead of the drag.
        Native,
        None,
    }

    thread_local! {
        static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
        static OWN: RefCell<OwnDrops> = RefCell::new(OwnDrops::default());
    }

    /// The window's handles, read afresh for every use.
    fn handles(
        window: &slint::winit_030::winit::window::Window,
    ) -> Option<(RawDisplayHandle, RawWindowHandle)> {
        Some((
            window.display_handle().ok()?.as_raw(),
            window.window_handle().ok()?.as_raw(),
        ))
    }

    /// Bind to the window's display once winit has created it (polled for
    /// up to five seconds: the window appears after the event loop starts;
    /// never with a non-winit backend). On Wayland the button-press serial a
    /// drag needs is recorded from then on.
    pub(crate) fn init(ui: &MainWindow) {
        init_within(ui, 100);
    }

    fn init_within(ui: &MainWindow, attempts: u32) {
        if BACKEND.with(|cell| cell.borrow().is_some()) {
            return;
        }
        let Some(handles) = ui.window().with_winit_window(handles) else {
            if attempts > 0 {
                let weak = ui.as_weak();
                slint::Timer::single_shot(Duration::from_millis(50), move || {
                    if let Some(ui) = weak.upgrade() {
                        init_within(&ui, attempts - 1);
                    }
                });
            }
            return;
        };
        let backend = match handles {
            #[cfg(target_os = "linux")]
            Some((RawDisplayHandle::Wayland(display), _)) => {
                // Safety: winit's display lives as long as the event loop;
                // `shutdown` stops our queue before the UI returns.
                unsafe { super::wayland::Wayland::new(display.display.as_ptr()) }
                    .map_or(Backend::None, Backend::Wayland)
            }
            Some(_) => Backend::Native,
            None => Backend::None,
        };
        BACKEND.with(|cell| *cell.borrow_mut() = Some(backend));
    }

    /// Start dragging `paths` out of the window. False when this platform or
    /// session cannot (then nothing happened and the press stays a click).
    /// `icon` renders the drag image when the platform shows ours.
    pub(crate) fn start(
        ui: &MainWindow,
        paths: Vec<PathBuf>,
        icon: impl FnOnce() -> Option<Icon>,
    ) -> bool {
        let Some((display, window)) = ui.window().with_winit_window(handles).flatten() else {
            return false;
        };
        OWN.with(|own| own.borrow_mut().dragged(&paths, Instant::now()));
        let weak = ui.as_weak();
        let done = move || {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    release_pointer(&ui);
                }
            });
        };
        BACKEND.with(|cell| match (cell.borrow().as_ref(), display, window) {
            #[cfg(target_os = "linux")]
            (Some(Backend::Wayland(wayland)), _, RawWindowHandle::Wayland(surface)) => {
                // Safety: the window's live surface, used during this call.
                unsafe {
                    wayland.start_drag(
                        surface.surface.as_ptr(),
                        Payload::new(paths),
                        icon(),
                        Box::new(done),
                    )
                }
            }
            #[cfg(target_os = "linux")]
            (
                Some(Backend::Native),
                RawDisplayHandle::Xlib(display),
                RawWindowHandle::Xlib(window),
            ) => {
                let _ = icon;
                display.display.is_some_and(|pointer| {
                    super::x11::start_drag(
                        pointer.as_ptr() as usize,
                        window.window as u32,
                        Payload::new(paths),
                        Box::new(done),
                    )
                })
            }
            #[cfg(windows)]
            (Some(Backend::Native), _, RawWindowHandle::Win32(window)) => {
                let _ = icon;
                let hwnd = window.hwnd.get();
                // OLE runs a modal loop until the drop; let the press event
                // finish dispatching first.
                slint::Timer::single_shot(Duration::ZERO, move || {
                    super::windows::drag(hwnd, &paths);
                    // winit delivers a drop on our own window after the
                    // modal loop: count the window from the drag's end.
                    OWN.with(|own| own.borrow_mut().dragged(&paths, Instant::now()));
                    done();
                });
                true
            }
            _ => {
                let _ = (icon, display);
                false
            }
        })
    }

    /// Ctrl+C on native Wayland: offer the files as the clipboard selection
    /// ourselves. False means "not handled here" (arboard does X11/Windows).
    pub(crate) fn copy(paths: &[PathBuf]) -> bool {
        BACKEND.with(|cell| match cell.borrow().as_ref() {
            #[cfg(target_os = "linux")]
            Some(Backend::Wayland(wayland)) => wayland.set_selection(Payload::new(paths.to_vec())),
            _ => {
                let _ = paths;
                false
            }
        })
    }

    pub(crate) fn is_own_drop(path: &Path) -> bool {
        OWN.with(|own| own.borrow().is_own(path, Instant::now()))
    }

    /// Stop the Wayland queue before winit's display goes away.
    pub(crate) fn shutdown() {
        BACKEND.with(|cell| cell.borrow_mut().take());
    }

    /// The system kept the button release for itself: tell Slint the press
    /// is over, away from every row, so no card stays pressed and nothing
    /// counts as a click.
    fn release_pointer(ui: &MainWindow) {
        use slint::platform::{PointerEventButton, WindowEvent};
        let away = slint::LogicalPosition::new(-1.0e5, -1.0e5);
        let window = ui.window();
        window.dispatch_event(WindowEvent::PointerReleased {
            position: away,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerExited);
    }
}

/// The XDG FileTransfer portal: register the files read-only and hand the
/// key to the receiver. Blocking; called only when a receiver asks for it.
#[cfg(target_os = "linux")]
fn portal_key(paths: &[PathBuf]) -> Option<String> {
    pollster::block_on(async {
        let transfer = ashpd::documents::FileTransfer::new().await.ok()?;
        let key = transfer.start_transfer(false, true).await.ok()?;
        let files = paths
            .iter()
            .map(std::fs::File::open)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        transfer.add_files(&key, &files).await.ok()?;
        Some(key)
    })
}

/// Whether the FileTransfer portal answers at all; only then is its type offered.
#[cfg(target_os = "linux")]
fn portal_available() -> bool {
    pollster::block_on(async {
        let Ok(transfer) = ashpd::documents::FileTransfer::new().await else {
            return false;
        };
        match transfer.start_transfer(false, true).await {
            Ok(key) => transfer.stop_transfer(&key).await.is_ok(),
            Err(_) => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, path: Option<&str>) -> DownloadView {
        let mut view: DownloadView = serde_json::from_value(serde_json::json!({
            "id": id, "gid": null, "url": "https://example.org/a", "filename": null,
            "dir": "/dl", "status": "complete", "kind": "http", "total_bytes": 0,
            "completed_bytes": 0, "download_speed": 0, "upload_speed": 0, "connections": 0,
            "num_seeders": 0, "referrer": null, "info_hash": null, "error_code": null,
            "error_message": null, "category_id": null, "created_at": 0, "finished_at": null,
            "page_url": null, "format_id": null, "speed_limit": null, "package_id": null,
            "mime": null, "checksum": null, "start_at": null, "media_opts": null
        }))
        .unwrap();
        view.path = path.map(str::to_owned);
        view
    }

    #[test]
    fn a_drag_starts_once_past_the_threshold_and_only_for_the_pressed_row() {
        let mut gesture = Gesture::default();
        assert!(!gesture.moved(1, 20.0, 0.0), "no press, no drag");
        gesture.press(1);
        assert!(
            !gesture.moved(1, 5.0, 5.0),
            "a 7 px wobble is still a click"
        );
        assert!(
            !gesture.moved(2, 30.0, 0.0),
            "another row's move does not count"
        );
        assert!(gesture.moved(1, 6.0, -6.0));
        assert!(!gesture.moved(1, 40.0, 0.0), "fires once per press");
        gesture.press(1);
        assert!(gesture.moved(1, 0.0, 8.0));
    }

    #[cfg_attr(
        not(unix),
        ignore = "Unix path literals; the URI list is only offered on Linux"
    )]
    #[test]
    fn a_drag_carries_the_selection_only_when_it_starts_on_a_selected_row() {
        let rows = [
            row(1, Some("/dl/a.iso")),
            row(2, None),
            row(3, Some("/dl/gone.zip")),
            row(4, Some("/dl/torrent folder")),
            row(5, Some("relative.txt")),
        ];
        let exists = |path: &Path| path != Path::new("/dl/gone.zip");
        let paths = |selection: &[i64], pressed| {
            drag_paths(&rows, &selection.iter().copied().collect(), pressed, exists)
        };
        let all = vec![
            PathBuf::from("/dl/a.iso"),
            PathBuf::from("/dl/torrent folder"),
        ];
        assert_eq!(
            paths(&[1, 2, 3, 4], 4),
            all,
            "selected: every finished, existing row"
        );
        assert_eq!(paths(&[1, 2, 3, 4], 1), all);
        assert_eq!(
            paths(&[1, 3], 4),
            vec![PathBuf::from("/dl/torrent folder")],
            "unselected: just it"
        );
        assert!(
            paths(&[1, 2], 2).is_empty(),
            "an unfinished row never drags"
        );
        assert!(paths(&[1, 3], 3).is_empty(), "nor one whose file is gone");
        assert!(paths(&[], 5).is_empty(), "nor a relative path");
        assert!(paths(&[], 9).is_empty());
    }

    #[cfg_attr(
        not(unix),
        ignore = "Unix path literals; the URI list is only offered on Linux"
    )]
    #[test]
    fn uri_lists_encode_every_awkward_byte() {
        let paths = [
            PathBuf::from("/home/me/My Files/100% done #1?.txt"),
            PathBuf::from("/tmp/line\nbreak"),
            PathBuf::from("/tmp/çay ☕/Ünïcödé.mkv"),
            PathBuf::from("/tmp/a-b_c.d~e"),
        ];
        assert_eq!(
            uri_list(&paths),
            "file:///home/me/My%20Files/100%25%20done%20%231%3F.txt\r\n\
             file:///tmp/line%0Abreak\r\n\
             file:///tmp/%C3%A7ay%20%E2%98%95/%C3%9Cn%C3%AFc%C3%B6d%C3%A9.mkv\r\n\
             file:///tmp/a-b_c.d~e\r\n"
        );
        for line in uri_list(&paths).lines() {
            let url = url::Url::parse(line).unwrap();
            assert!(
                paths.contains(&url.to_file_path().unwrap()),
                "{line} round-trips"
            );
        }
    }

    #[test]
    fn payloads_offer_uris_text_and_the_gnome_copy_list() {
        let payload = Payload::new(vec![PathBuf::from("/a b"), PathBuf::from("/c")]);
        assert_eq!(
            payload.data(URI_LIST).unwrap(),
            b"file:///a%20b\r\nfile:///c\r\n"
        );
        assert_eq!(payload.data(TEXT_UTF8).unwrap(), b"/a b\n/c");
        assert_eq!(payload.data(TEXT).unwrap(), b"/a b\n/c");
        assert_eq!(
            payload.data(GNOME_COPIED).unwrap(),
            b"copy\nfile:///a%20b\nfile:///c"
        );
        assert!(payload.data("image/png").is_none());
        for mime in Payload::DRAG_TYPES.iter().chain(&Payload::COPY_TYPES) {
            assert!(payload.data(mime).is_some(), "{mime} is served");
        }
    }

    #[test]
    fn only_a_recent_drop_of_our_own_files_is_ours() {
        let start = Instant::now();
        let mut own = OwnDrops::default();
        assert!(!own.is_own(Path::new("/a"), start));
        own.dragged(&[PathBuf::from("/a")], start);
        assert!(own.is_own(Path::new("/a"), start + Duration::from_secs(1)));
        assert!(!own.is_own(Path::new("/b"), start));
        assert!(!own.is_own(Path::new("/a"), start + Duration::from_secs(4)));
    }
}
