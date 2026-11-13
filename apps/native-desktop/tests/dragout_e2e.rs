//! Drag-out end to end, without touching the desktop it runs on: a nested
//! KWin (`--virtual` output plus its own Xwayland and D-Bus), an isolated
//! app instance with one finished download whose name is awkward, a drop
//! target helper (an XDND window, or a Wayland layer-shell surface), and
//! the pointer of the nested KWin driven through its fake-input protocol
//! (`xdotool` only reads the X11 window's position).
//!
//! ```sh
//! cargo test -p minidl-native-desktop --test dragout_e2e -- --ignored --test-threads 1
//! ```
//!
//! Skips (passes with a notice) when kwin_wayland, Xwayland, xdotool,
//! dbus-run-session or aria2c is missing.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const APP: &str = env!("CARGO_BIN_EXE_mini-downloader");
const NAME: &str = "Drag 100% #1 ça?y ☕ [x].txt";
/// The output is 1600x900; the target covers the right edge.
const SCREEN: (i32, i32) = (1600, 900);
const TARGET_WIDTH: i32 = 350;
const KEY_ESC: u32 = 1;

#[test]
#[ignore = "starts a nested KWin; needs kwin_wayland, Xwayland, xdotool and aria2c"]
fn an_x11_drag_reaches_an_xdnd_target() {
    run(Mode::X11, End::Drop);
}

#[test]
#[ignore = "starts a nested KWin; needs kwin_wayland, Xwayland, xdotool and aria2c"]
fn a_wayland_drag_reaches_a_data_device_target() {
    run(Mode::Wayland, End::Drop);
}

#[test]
#[ignore = "starts a nested KWin; needs kwin_wayland, Xwayland, xdotool and aria2c"]
fn escape_and_dropping_on_the_app_itself_do_nothing() {
    // Not Escape under X11: on Xwayland KWin mirrors an X drag as its own
    // Wayland drag and keeps the Escape key for that (the X source's
    // keyboard grab sees it only on a real X server).
    run(Mode::X11, End::OwnWindow);
    run(Mode::Wayland, End::Escape);
    run(Mode::Wayland, End::OwnWindow);
}

/// Re-entered by the tests above as a separate process: the drop target.
#[test]
#[ignore = "helper process for the drag-out tests"]
fn drop_target_helper() {
    let Ok(kind) = std::env::var("MINIDL_DROP_TARGET") else {
        return;
    };
    let out = PathBuf::from(std::env::var_os("MINIDL_DROP_OUT").unwrap());
    match kind.as_str() {
        "x11" => x11_target::run(&out),
        _ => wayland_target::run(&out),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    X11,
    Wayland,
}

/// How the drag ends: on the target, with Escape over it, or released
/// back on the app's own window.
#[derive(Clone, Copy, PartialEq)]
enum End {
    Drop,
    Escape,
    OwnWindow,
}

fn run(mode: Mode, end: End) {
    let missing = [
        "kwin_wayland",
        "Xwayland",
        "xdotool",
        "dbus-run-session",
        "aria2c",
    ]
    .into_iter()
    .filter(|tool| !on_path(tool))
    .collect::<Vec<_>>();
    if !missing.is_empty() {
        eprintln!(
            "skipped: drag-out end-to-end test needs {}",
            missing.join(", ")
        );
        return;
    }
    let mut nest = Nest::start();
    let file = nest.finished_download(mode);
    assert!(file.ends_with(NAME.replace('?', "_")), "{file:?}");

    let out = nest.root.join("dropped");
    let mut target = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "drop_target_helper", "--nocapture"])
        .envs(nest.env(mode))
        .env(
            "MINIDL_DROP_TARGET",
            if mode == Mode::X11 { "x11" } else { "wayland" },
        )
        .env("MINIDL_DROP_OUT", &out)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let ready = out.with_extension("ready");
    wait_for(|| ready.exists(), "the drop target to map");

    nest.ctl(mode, "show", None).expect("show the window");
    let window = nest.app_window(mode);
    // The first card, between its title row and its action rail.
    let (x, y) = (window.0 + 600, window.1 + 165);
    let goal = match end {
        End::OwnWindow => (window.0 + 500, window.1 + 400),
        _ => (SCREEN.0 - TARGET_WIDTH / 2, SCREEN.1 / 2),
    };
    let mut input = Input::connect(&nest.root.join("r/wl"));
    input.move_to(x, y);
    std::thread::sleep(Duration::from_millis(500));
    input.button(true);
    for step in 1..=4 {
        std::thread::sleep(Duration::from_millis(60));
        input.move_to(x + step * 4, y + step);
    }
    for step in 1..=10 {
        std::thread::sleep(Duration::from_millis(60));
        let at = |from: i32, to: i32| from + (to - from) * step / 10;
        input.move_to(at(x, goal.0), at(y, goal.1));
    }
    std::thread::sleep(Duration::from_millis(400));
    input.move_to(goal.0 + 3, goal.1);
    std::thread::sleep(Duration::from_millis(400));
    if end == End::Escape {
        input.key(KEY_ESC);
        std::thread::sleep(Duration::from_millis(300));
    }
    input.button(false);

    let expected = format!("file://{}\r\n", percent_path(&file));
    if end == End::Drop {
        wait_for(|| out.exists(), "the drop to arrive");
        let _ = target.wait();
        let received = std::fs::read(&out).unwrap();
        assert_eq!(String::from_utf8_lossy(&received), expected);
        let lines = String::from_utf8(received).unwrap();
        let url = url::Url::parse(lines.trim_end()).unwrap();
        assert_eq!(
            url.to_file_path().unwrap(),
            file,
            "the receiver gets the very path"
        );
        let action = std::fs::read_to_string(out.with_extension("action")).unwrap();
        assert_eq!(action, "copy", "a drop may only copy");
    } else {
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!out.exists(), "nothing was dropped");
        let _ = target.kill();
        let _ = target.wait();
        let list = nest
            .ctl(mode, "list_downloads", Some(r#"{"status":null}"#))
            .unwrap();
        let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert_eq!(
            rows.as_array().map(Vec::len),
            Some(1),
            "no download was added"
        );
    }

    // Afterwards the list is in a normal state: the drag selected nothing
    // (Ctrl+C copies no file), and a plain click selects the row at once
    // (then Ctrl+C puts it on the clipboard, natively on Wayland).
    let mut clipboard = Clipboard::connect(&nest.root.join("r/wl"));
    let before = clipboard.uri_list();
    input.move_to(x, y);
    std::thread::sleep(Duration::from_millis(300));
    input.copy();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(clipboard.uri_list(), before, "a drag never selects the row");
    input.button(true);
    input.button(false);
    std::thread::sleep(Duration::from_millis(300));
    input.copy();
    let mut copied = None;
    wait_for(
        || {
            copied = clipboard.uri_list();
            copied.is_some() && copied != before
        },
        "Ctrl+C to put the file on the clipboard",
    );
    // Under X11 arboard writes the list without the final CRLF.
    assert_eq!(copied.unwrap().trim_end(), expected.trim_end());
}

/// RFC 3986: everything but unreserved characters and `/` encoded.
fn percent_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (*byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

fn wait_for(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The nested session and the isolated app; everything dies with it.
struct Nest {
    root: PathBuf,
    inner: Vec<(String, String)>,
    kwin: Child,
    app: Option<Child>,
}

impl Nest {
    fn start() -> Self {
        // Short: a Unix socket path is limited to 108 bytes.
        let root = PathBuf::from(format!("/tmp/mdl-dnd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["r", "home", "srv"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::set_permissions(
            root.join("r"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let child = root.join("child.sh");
        std::fs::write(
            &child,
            format!(
                // KWin runs its child in a session of its own: record the pid.
                "#!/bin/sh\necho $$ > '{0}/child.pid'\nenv > '{0}/kwin-env'\nexec sleep 100000\n",
                root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&child, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let kwin = Command::new("dbus-run-session")
            .args([
                "--",
                "kwin_wayland",
                "--virtual",
                "--xwayland",
                "--socket",
                "wl",
            ])
            .args([
                "--width",
                &SCREEN.0.to_string(),
                "--height",
                &SCREEN.1.to_string(),
            ])
            .arg(&child)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .envs(Self::isolated(&root))
            .env("KWIN_WAYLAND_NO_PERMISSION_CHECKS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let env = root.join("kwin-env");
        wait_for(
            || std::fs::metadata(&env).is_ok_and(|meta| meta.len() > 0),
            "the nested KWin",
        );
        std::thread::sleep(Duration::from_millis(300));
        let inner = std::fs::read_to_string(&env)
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter(|(key, _)| {
                ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"].contains(key)
            })
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        let display = inner
            .iter()
            .find(|(key, _)| key == "DISPLAY")
            .map(|(_, value)| value.clone());
        assert!(
            display.is_some() && display != std::env::var("DISPLAY").ok(),
            "the nested Xwayland must not be the desktop's display"
        );
        Self {
            root,
            inner,
            kwin,
            app: None,
        }
    }

    fn isolated(root: &Path) -> Vec<(String, String)> {
        [
            ("HOME", "home"),
            ("XDG_RUNTIME_DIR", "r"),
            ("XDG_CONFIG_HOME", "c"),
            ("XDG_DATA_HOME", "d"),
            ("XDG_CACHE_HOME", "ca"),
        ]
        .into_iter()
        .map(|(key, dir)| (key.to_owned(), root.join(dir).display().to_string()))
        .chain([("LANG".to_owned(), "C.UTF-8".to_owned())])
        .collect()
    }

    /// The nested session's environment; the X11 mode hides Wayland.
    fn env(&self, mode: Mode) -> Vec<(String, String)> {
        let mut env = Self::isolated(&self.root);
        env.extend(self.inner.iter().cloned());
        env.push(("MINIDL_BENCHMARK".into(), "1".into()));
        if mode == Mode::X11 {
            env.push(("MINIDL_UI_X11".into(), "1".into()));
        }
        env
    }

    fn command(&self, mode: Mode, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .envs(self.env(mode));
        command
    }

    /// One `ctl` command; None when the controller is not there or refused.
    fn ctl(&self, mode: Mode, name: &str, args: Option<&str>) -> Option<String> {
        let mut command = self.command(mode, APP);
        command.args(["ctl", name]).args(args);
        let output = command.output().unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Serve one awkwardly named file on loopback and download it.
    fn finished_download(&mut self, mode: Mode) -> PathBuf {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = [0_u8; 4096];
                let _ = stream.read(&mut head);
                let body = b"dragged out\n";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        // The controller passes MINIDL_UI_X11 on to the UI it spawns.
        let app = self
            .command(mode, APP)
            .arg("--background")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.app = Some(app);
        wait_for(|| self.ctl(mode, "ping", None).is_some(), "the controller");
        let url = format!(
            "http://127.0.0.1:{port}/{}",
            percent_path(Path::new(NAME)).replace('/', "%2F")
        );
        self.ctl(
            mode,
            "ingest_url",
            Some(&format!(r#"{{"url":"{url}","checksum":null}}"#)),
        )
        .expect("ingest the loopback URL");
        let mut path = None;
        wait_for(
            || {
                let list = self
                    .ctl(mode, "list_downloads", Some(r#"{"status":null}"#))
                    .unwrap_or_default();
                let rows: serde_json::Value = serde_json::from_str(&list).unwrap_or_default();
                path = rows
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|row| row["path"].as_str())
                    .map(PathBuf::from);
                path.is_some()
            },
            "the download to finish",
        );
        path.unwrap()
    }

    /// The app window's top-left on the screen. KWin centres a new window;
    /// under Xwayland we can ask, natively we assume the same placement.
    fn app_window(&self, mode: Mode) -> (i32, i32) {
        if mode == Mode::Wayland {
            // No X window to ask: give the UI time to map, then use KWin's
            // centred placement of the 1000x680 window below its title bar.
            std::thread::sleep(Duration::from_secs(3));
            return ((SCREEN.0 - 1000) / 2, (SCREEN.1 - 680) / 2 + 18);
        }
        let position = || {
            let output = self
                .command(Mode::X11, "xdotool")
                .args([
                    "search",
                    "--name",
                    "Mini Downloader",
                    "getwindowgeometry",
                    "--shell",
                ])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let value = |key: &str| {
                text.lines()
                    .find_map(|line| line.strip_prefix(key)?.parse::<i32>().ok())
            };
            value("X=").zip(value("Y="))
        };
        // Mapped at 0,0 first, then placed by KWin: wait until it stays put.
        let mut found = None;
        wait_for(
            || {
                let before = position();
                std::thread::sleep(Duration::from_millis(500));
                found = position();
                found.is_some() && found == before && found != Some((0, 0))
            },
            "the app window to be placed",
        );
        found.unwrap()
    }
}

impl Drop for Nest {
    fn drop(&mut self) {
        let _ = self.command(Mode::X11, APP).args(["ctl", "quit"]).output();
        if let Some(app) = self.app.as_mut() {
            let _ = app.kill();
            let _ = app.wait();
        }
        if let Ok(pid) = std::fs::read_to_string(self.root.join("child.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.kwin.id())])
            .status();
        let _ = self.kwin.wait();
        // KWin saves its output config on the way out: wait for the group.
        let group = format!("-{}", self.kwin.id());
        for _ in 0..50 {
            let alive = Command::new("kill")
                .args(["-0", "--", &group])
                .stderr(Stdio::null())
                .status();
            if !alive.is_ok_and(|status| status.success()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

use std::os::unix::process::CommandExt as _;

/// KWin's `org_kde_kwin_fake_input` (allowed by
/// KWIN_WAYLAND_NO_PERMISSION_CHECKS in the nest): exact pointer moves and
/// button presses that reach X11 and Wayland clients alike.
struct Input {
    conn: wayland_client::Connection,
    queue: wayland_client::EventQueue<Globals>,
    fake: fake_input::OrgKdeKwinFakeInput,
}

use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input as fake_input;

#[derive(Default)]
struct Globals {
    fake: Option<fake_input::OrgKdeKwinFakeInput>,
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &wayland_client::Connection,
        handle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "org_kde_kwin_fake_input" {
                state.fake = Some(registry.bind(name, version.min(4), handle, ()));
            }
        }
    }
}

impl wayland_client::Dispatch<fake_input::OrgKdeKwinFakeInput, ()> for Globals {
    fn event(
        _: &mut Self,
        _: &fake_input::OrgKdeKwinFakeInput,
        _: fake_input::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Input {
    fn connect(socket: &Path) -> Self {
        let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let conn = wayland_client::Connection::from_socket(stream).unwrap();
        let mut queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let mut globals = Globals::default();
        queue.roundtrip(&mut globals).unwrap();
        let fake = globals.fake.expect("KWin offers org_kde_kwin_fake_input");
        fake.authenticate("dragout e2e".into(), "drive the nested pointer".into());
        Self { conn, queue, fake }
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.fake
            .pointer_motion_absolute(f64::from(x), f64::from(y));
        self.queue.roundtrip(&mut Globals::default()).unwrap();
    }

    /// Ctrl+C on the focused window.
    fn copy(&mut self) {
        const KEY_LEFTCTRL: u32 = 29;
        const KEY_C: u32 = 46;
        for (key, pressed) in [
            (KEY_LEFTCTRL, true),
            (KEY_C, true),
            (KEY_C, false),
            (KEY_LEFTCTRL, false),
        ] {
            self.fake.keyboard_key(key, u32::from(pressed));
            self.queue.roundtrip(&mut Globals::default()).unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn key(&mut self, key: u32) {
        for pressed in [true, false] {
            self.fake.keyboard_key(key, u32::from(pressed));
            self.queue.roundtrip(&mut Globals::default()).unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn button(&mut self, pressed: bool) {
        const BTN_LEFT: u32 = 0x110;
        self.fake.button(BTN_LEFT, u32::from(pressed));
        self.queue.roundtrip(&mut Globals::default()).unwrap();
        let _ = &self.conn;
    }
}

/// Reads the clipboard the way a clipboard manager does (ext data-control),
/// so the check needs no keyboard focus of its own.
struct Clipboard {
    conn: wayland_client::Connection,
    queue: wayland_client::EventQueue<ClipboardState>,
    state: ClipboardState,
}

use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as control_device, ext_data_control_manager_v1 as control_manager,
    ext_data_control_offer_v1 as control_offer,
};

#[derive(Default)]
struct ClipboardState {
    manager: Option<control_manager::ExtDataControlManagerV1>,
    seat: Option<wayland_client::protocol::wl_seat::WlSeat>,
    offer: Option<(control_offer::ExtDataControlOfferV1, Vec<String>)>,
    selection: Option<control_offer::ExtDataControlOfferV1>,
}

impl Clipboard {
    fn connect(socket: &Path) -> Self {
        let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let conn = wayland_client::Connection::from_socket(stream).unwrap();
        let mut queue = conn.new_event_queue();
        let handle = queue.handle();
        conn.display().get_registry(&handle, ());
        let mut state = ClipboardState::default();
        queue.roundtrip(&mut state).unwrap();
        let manager = state.manager.clone().expect("KWin offers ext data-control");
        manager.get_data_device(state.seat.as_ref().unwrap(), &handle, ());
        queue.roundtrip(&mut state).unwrap();
        Self { conn, queue, state }
    }

    /// The current selection's text/uri-list, if it has one.
    fn uri_list(&mut self) -> Option<String> {
        self.queue.roundtrip(&mut self.state).unwrap();
        let offer = self.state.selection.as_ref()?;
        let (mut reader, writer) = std::io::pipe().unwrap();
        offer.receive("text/uri-list".into(), std::os::fd::AsFd::as_fd(&writer));
        drop(writer);
        self.conn.flush().unwrap();
        let mut text = String::new();
        reader.read_to_string(&mut text).ok()?;
        Some(text).filter(|text| !text.is_empty())
    }
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()>
    for ClipboardState
{
    fn event(
        state: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &wayland_client::Connection,
        handle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "ext_data_control_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, handle, ()))
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, 1, handle, ()))
                }
                _ => {}
            }
        }
    }
}

impl wayland_client::Dispatch<control_device::ExtDataControlDeviceV1, ()> for ClipboardState {
    fn event(
        state: &mut Self,
        _: &control_device::ExtDataControlDeviceV1,
        event: control_device::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
        match event {
            control_device::Event::DataOffer { id } => state.offer = Some((id, Vec::new())),
            control_device::Event::Selection { id } => {
                // Only a selection that offers a URI list counts.
                let offered = state.offer.take().filter(|(offer, types)| {
                    Some(offer) == id.as_ref() && types.iter().any(|mime| mime == "text/uri-list")
                });
                state.selection = offered.map(|(offer, _)| offer);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipboardState, control_device::ExtDataControlDeviceV1, [
        control_device::EVT_DATA_OFFER_OPCODE => (control_offer::ExtDataControlOfferV1, ()),
    ]);
}

impl wayland_client::Dispatch<control_offer::ExtDataControlOfferV1, ()> for ClipboardState {
    fn event(
        state: &mut Self,
        offer: &control_offer::ExtDataControlOfferV1,
        event: control_offer::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
        let control_offer::Event::Offer { mime_type } = event else {
            return;
        };
        if let Some((current, types)) = state.offer.as_mut() {
            if current == offer {
                types.push(mime_type);
            }
        }
    }
}

impl wayland_client::Dispatch<control_manager::ExtDataControlManagerV1, ()> for ClipboardState {
    fn event(
        _: &mut Self,
        _: &control_manager::ExtDataControlManagerV1,
        _: control_manager::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_seat::WlSeat, ()> for ClipboardState {
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_seat::WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &wayland_client::Connection,
        _: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

/// An XDND version 5 target window over the right edge of the screen.
mod x11_target {
    use std::path::Path;

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode,
        WindowClass,
    };
    use x11rb::protocol::Event;
    use x11rb::wrapper::ConnectionExt as _;

    x11rb::atom_manager! {
        Atoms: AtomsCookie {
            XdndAware,
            XdndSelection,
            XdndEnter,
            XdndPosition,
            XdndStatus,
            XdndDrop,
            XdndFinished,
            XdndActionCopy,
            URI_LIST: b"text/uri-list",
            DROPPED,
        }
    }

    pub(super) fn run(out: &Path) {
        let (conn, screen) = x11rb::connect(None).unwrap();
        let screen = &conn.setup().roots[screen];
        let atoms = Atoms::new(&conn).unwrap().reply().unwrap();
        let window = conn.generate_id().unwrap();
        let (width, height) = super::SCREEN;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            (width - super::TARGET_WIDTH) as i16,
            0,
            super::TARGET_WIDTH as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(screen.white_pixel)
                .override_redirect(1)
                .event_mask(EventMask::PROPERTY_CHANGE),
        )
        .unwrap();
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms.XdndAware,
            AtomEnum::ATOM,
            &[5],
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(out.with_extension("ready"), "").unwrap();
        let mut source = 0;
        let mut action = 0;
        loop {
            match conn.wait_for_event().unwrap() {
                Event::ClientMessage(event) if event.type_ == atoms.XdndEnter => {
                    source = event.data.as_data32()[0];
                }
                Event::ClientMessage(event) if event.type_ == atoms.XdndPosition => {
                    action = event.data.as_data32()[4];
                    let status = [window, 1, 0, 0, atoms.XdndActionCopy];
                    let reply = ClientMessageEvent::new(32, source, atoms.XdndStatus, status);
                    conn.send_event(false, source, EventMask::NO_EVENT, reply)
                        .unwrap();
                    conn.flush().unwrap();
                }
                Event::ClientMessage(event) if event.type_ == atoms.XdndDrop => {
                    let time = event.data.as_data32()[2];
                    conn.convert_selection(
                        window,
                        atoms.XdndSelection,
                        atoms.URI_LIST,
                        atoms.DROPPED,
                        time,
                    )
                    .unwrap();
                    conn.flush().unwrap();
                }
                Event::SelectionNotify(event) if event.property == atoms.DROPPED => {
                    let data = conn
                        .get_property(true, window, atoms.DROPPED, AtomEnum::ANY, 0, 1 << 20)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .value;
                    let finished = [window, 1, atoms.XdndActionCopy, 0, 0];
                    let reply = ClientMessageEvent::new(32, source, atoms.XdndFinished, finished);
                    conn.send_event(false, source, EventMask::NO_EVENT, reply)
                        .unwrap();
                    conn.flush().unwrap();
                    let name = if action == atoms.XdndActionCopy {
                        "copy"
                    } else {
                        "other"
                    };
                    std::fs::write(out.with_extension("action"), name).unwrap();
                    std::fs::write(out, data).unwrap();
                    return;
                }
                _ => {}
            }
        }
    }
}

/// A layer-shell surface over the right edge that accepts text/uri-list drops.
mod wayland_target {
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::path::{Path, PathBuf};

    use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
    use wayland_client::protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_data_device,
        wl_data_device::WlDataDevice, wl_data_offer, wl_data_offer::WlDataOffer, wl_registry,
        wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    };
    use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};
    use wayland_protocols_wlr::layer_shell::v1::client::{
        zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
    };

    #[derive(Default)]
    struct Target {
        out: PathBuf,
        compositor: Option<WlCompositor>,
        shm: Option<WlShm>,
        layers: Option<ZwlrLayerShellV1>,
        seat: Option<WlSeat>,
        manager: Option<WlDataDeviceManager>,
        surface: Option<WlSurface>,
        offer: Option<WlDataOffer>,
        action: Option<DndAction>,
        done: bool,
    }

    pub(super) fn run(out: &Path) {
        let conn = Connection::connect_to_env().unwrap();
        let mut queue = conn.new_event_queue();
        let handle = queue.handle();
        conn.display().get_registry(&handle, ());
        let mut target = Target {
            out: out.to_owned(),
            ..Target::default()
        };
        queue.roundtrip(&mut target).unwrap();
        let surface = target
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&handle, ());
        let layer = target.layers.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "drop-target".into(),
            &handle,
            (),
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Right);
        layer.set_size(super::TARGET_WIDTH as u32, 0);
        surface.commit();
        target.surface = Some(surface);
        let manager = target.manager.clone().unwrap();
        manager.get_data_device(target.seat.as_ref().unwrap(), &handle, ());
        while !target.done {
            queue.blocking_dispatch(&mut target).unwrap();
        }
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for Target {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            handle: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                match interface.as_str() {
                    "wl_compositor" => state.compositor = Some(registry.bind(name, 4, handle, ())),
                    "wl_shm" => state.shm = Some(registry.bind(name, 1, handle, ())),
                    "zwlr_layer_shell_v1" => {
                        state.layers = Some(registry.bind(name, version.min(4), handle, ()))
                    }
                    "wl_seat" if state.seat.is_none() => {
                        state.seat = Some(registry.bind(name, 5, handle, ()))
                    }
                    "wl_data_device_manager" => {
                        state.manager = Some(registry.bind(name, 3, handle, ()))
                    }
                    _ => {}
                }
            }
        }
    }

    impl Dispatch<ZwlrLayerSurfaceV1, ()> for Target {
        fn event(
            state: &mut Self,
            layer: &ZwlrLayerSurfaceV1,
            event: zwlr_layer_surface_v1::Event,
            _: &(),
            _: &Connection,
            handle: &QueueHandle<Self>,
        ) {
            let zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } = event
            else {
                return;
            };
            layer.ack_configure(serial);
            let (width, height) = (width.max(1) as i32, height.max(1) as i32);
            let file = tempfile_with(&vec![0xff; (width * height * 4) as usize]);
            let pool = state.shm.as_ref().unwrap().create_pool(
                file.as_fd(),
                width * height * 4,
                handle,
                (),
            );
            let buffer = pool.create_buffer(
                0,
                width,
                height,
                width * 4,
                wl_shm::Format::Xrgb8888,
                handle,
                (),
            );
            let surface = state.surface.as_ref().unwrap();
            surface.attach(Some(&buffer), 0, 0);
            surface.damage(0, 0, width, height);
            surface.commit();
            std::fs::write(state.out.with_extension("ready"), "").unwrap();
        }
    }

    fn tempfile_with(bytes: &[u8]) -> std::fs::File {
        let path = std::env::temp_dir().join(format!("mdl-dnd-shm-{}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        file
    }

    impl Dispatch<WlDataDevice, ()> for Target {
        fn event(
            state: &mut Self,
            _: &WlDataDevice,
            event: wl_data_device::Event,
            _: &(),
            conn: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wl_data_device::Event::Enter {
                    serial,
                    id: Some(offer),
                    ..
                } => {
                    offer.accept(serial, Some("text/uri-list".into()));
                    offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Move);
                    state.offer = Some(offer);
                }
                wl_data_device::Event::Drop => {
                    let offer = state.offer.take().unwrap();
                    let (mut reader, writer) = std::io::pipe().unwrap();
                    offer.receive("text/uri-list".into(), writer.as_fd());
                    drop(writer);
                    conn.flush().unwrap();
                    let mut data = Vec::new();
                    reader.read_to_end(&mut data).unwrap();
                    offer.finish();
                    offer.destroy();
                    conn.flush().unwrap();
                    let action = match state.action {
                        Some(action) if action == DndAction::Copy => "copy",
                        _ => "other",
                    };
                    std::fs::write(state.out.with_extension("action"), action).unwrap();
                    std::fs::write(&state.out, data).unwrap();
                    state.done = true;
                }
                _ => {}
            }
        }

        event_created_child!(Target, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
        ]);
    }

    impl Dispatch<WlDataOffer, ()> for Target {
        fn event(
            state: &mut Self,
            _: &WlDataOffer,
            event: wl_data_offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // The compositor's choice from the source's actions and ours.
            if let wl_data_offer::Event::Action {
                dnd_action: WEnum::Value(action),
            } = event
            {
                state.action = Some(action);
            }
        }
    }

    macro_rules! ignore {
        ($($interface:ty),*) => {$(
            impl Dispatch<$interface, ()> for Target {
                fn event(_: &mut Self, _: &$interface, _: <$interface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
            }
        )*};
    }
    ignore!(
        WlCompositor,
        WlShm,
        WlShmPool,
        WlBuffer,
        WlSurface,
        WlSeat,
        WlDataDeviceManager,
        ZwlrLayerShellV1
    );
}
