use super::*;

const USAGE: &str = "\
Mini Downloader

Usage:
  mini-downloader [URL|FILE]...    add links, magnets, .torrent or .metalink files
  mini-downloader --background     start in the tray without opening a window
  mini-downloader --show | --hide | --quit
  mini-downloader ctl <command> [json-args]
                                   run one controller command, print its JSON result
  mini-downloader ctl subscribe    stream controller events, one JSON object per line
  mini-downloader ctl why '{\"id\":3}' [--json]
                                   why a download is in its state (text, or JSON)
  mini-downloader --version | --help

ctl examples:
  mini-downloader ctl snapshot
  mini-downloader ctl ingest_url '{\"url\":\"https://example.org/file.iso\"}'
  mini-downloader ctl pause '{\"id\":3}'

Commands are the snake_case variants of ControlCommand in
crates/minidl-ipc/src/lib.rs; an unknown command prints the full list.
ctl exits 0 on success, 1 when the controller reports an error, 2 on bad usage.
";

/// `ctl <command> [json-args]`: the scripting and agent surface. It speaks the
/// same user-private control socket as the UI, so it grants nothing new.
fn run_ctl(runtime: &tokio::runtime::Runtime, arguments: &[String]) -> i32 {
    use std::io::Write as _;

    let Some(name) = arguments.first() else {
        eprint!("{USAGE}");
        return 2;
    };
    if name == "subscribe" {
        let ended = runtime.block_on(ControlClient::subscribe(|event| {
            let line = serde_json::to_string(&event).unwrap_or_default();
            // A closed pipe (`| head`) is the reader saying it has enough.
            if writeln!(std::io::stdout(), "{line}").is_err() {
                std::process::exit(0);
            }
        }));
        eprintln!("event stream ended: {}", ended.err().unwrap_or_default());
        return 1;
    }
    let json_output = arguments
        .iter()
        .skip(1)
        .any(|argument| argument == "--json");
    let mut request = serde_json::json!({ "command": name });
    if let Some(raw) = arguments.get(1).filter(|argument| *argument != "--json") {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(arguments) => request["args"] = arguments,
            Err(error) => {
                eprintln!("arguments are not valid JSON: {error}");
                return 2;
            }
        }
    }
    let command = match serde_json::from_value::<ControlCommand>(request) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    let outcome = runtime.block_on(async {
        let client = ControlClient::connect().await.map_err(|error| {
            format!(
                "controller is not running ({error}); start it with `mini-downloader --background`"
            )
        })?;
        client.request(command).await
    });
    match outcome {
        Ok(result) => {
            match result.get("summary").and_then(|summary| summary.as_str()) {
                Some(summary) if name == "why" && !json_output => print!("{summary}"),
                _ => println!("{result}"),
            }
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

pub(super) fn run() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("mini-downloader {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{USAGE}");
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // aria2/yt-dlp do the heavy work in child processes. One async worker
        // is enough for sockets, timers and UI snapshots and saves an idle OS
        // thread in both controller and renderer processes.
        .worker_threads(1)
        .enable_all()
        .thread_name("minidl-worker")
        .build()
        .expect("create runtime");
    // Only as the first argument: a URL-scheme launch always puts the link
    // first, so a crafted link can never reach this.
    if arguments.first().is_some_and(|argument| argument == "ctl") {
        std::process::exit(run_ctl(&runtime, &arguments[1..]));
    }
    if let Some(command) = arguments
        .iter()
        .find_map(|argument| match argument.as_str() {
            "--quit" => Some(ControlCommand::Quit),
            "--hide" => Some(ControlCommand::Hide),
            "--show" => Some(ControlCommand::Show),
            _ => None,
        })
    {
        match runtime.block_on(server::send_one(command.clone())) {
            Ok(_) => return,
            Err(error) if matches!(command, ControlCommand::Show) => {
                eprintln!("no running controller; starting one: {error}");
            }
            Err(error) => {
                eprintln!("controller unavailable: {error}");
                return;
            }
        }
    }
    let ui_client = arguments.iter().any(|argument| argument == "--ui-client");
    if ui_client {
        // Slint's Linux accessibility bridge uses zbus, which discovers the
        // active Tokio reactor while constructing the window.
        let _runtime_guard = runtime.enter();
        // With `ui-mcp`, SLINT_BACKEND=headless runs the UI without a display.
        #[cfg(any(target_os = "linux", windows))]
        if cfg!(not(feature = "ui-mcp")) || std::env::var_os("SLINT_BACKEND").is_none() {
            if let Err(error) = select_ui_backend() {
                eprintln!("native UI backend: {error}");
                return;
            }
        }
        if let Err(error) = run_ui(&runtime) {
            eprintln!("native UI: {error}");
        }
    } else {
        let launch_commands = arguments
            .iter()
            .filter(|argument| !argument.starts_with("--"))
            .filter_map(|argument| command_for_argument(argument))
            .collect();
        if let Err(error) = runtime.block_on(run_controller(launch_commands)) {
            eprintln!("controller: {error}");
        }
    }
}

/// Windows: the frameless window keeps the system's shadow (and with it the
/// resize border and snap layouts' feel).
#[cfg(windows)]
fn select_ui_backend() -> Result<(), String> {
    use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows as _;
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|attributes| attributes.with_undecorated_shadow(true))
        .select()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn select_ui_backend() -> Result<(), String> {
    use slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland as WaylandAttributes;
    use slint::winit_030::winit::platform::x11::WindowAttributesExtX11 as X11Attributes;

    // Must match winit's own choice: Wayland whenever WAYLAND_DISPLAY is set
    // (the controller removes it for MINIDL_UI_X11=1).
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(move |attributes| {
            if wayland {
                WaylandAttributes::with_name(attributes, "io.minidownloader.app", "mini-downloader")
            } else {
                X11Attributes::with_name(attributes, "mini-downloader", "mini-downloader")
            }
        })
        .select()
        .map_err(|error| error.to_string())
}

pub(crate) fn command_for_path(path: &std::path::Path) -> Option<ControlCommand> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let kind = match extension.as_str() {
        "torrent" => minidl_ipc::DownloadKind::Torrent,
        "meta4" | "metalink" => minidl_ipc::DownloadKind::Metalink,
        _ => return None,
    };
    Some(ControlCommand::IngestFile {
        path: path.to_string_lossy().into_owned(),
        kind,
    })
}

/// Command for the Add field. The launch-argument parser builds link commands
/// without a checksum, so the one typed next to the link is attached here.
pub(crate) fn command_for_add_field(raw: &str, checksum: &str) -> ControlCommand {
    let raw = raw.trim();
    let checksum = (!checksum.trim().is_empty()).then(|| checksum.trim().to_owned());
    match command_for_argument(raw) {
        Some(ControlCommand::IngestUrl { url, .. }) => ControlCommand::IngestUrl { url, checksum },
        Some(command) => command,
        None => ControlCommand::IngestUrl {
            url: raw.to_owned(),
            checksum,
        },
    }
}

pub(crate) fn command_for_argument(argument: &str) -> Option<ControlCommand> {
    if let Ok(url) = url::Url::parse(argument) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .ok()
                .as_deref()
                .and_then(command_for_path);
        }
        if matches!(url.scheme(), "http" | "https" | "ftp" | "magnet") {
            return Some(ControlCommand::IngestUrl {
                url: argument.to_owned(),
                checksum: None,
            });
        }
        if url.scheme() == "minidownloader" {
            return url
                .query_pairs()
                .find(|(key, _)| key == "url")
                .and_then(|(_, value)| command_for_argument(&value));
        }
    }
    command_for_path(std::path::Path::new(argument))
}
