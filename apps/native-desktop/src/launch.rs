use super::*;

pub(super) fn run() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("mini-downloader {}", env!("CARGO_PKG_VERSION"));
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
        #[cfg(target_os = "linux")]
        if let Err(error) = select_linux_ui_backend() {
            eprintln!("native UI backend: {error}");
            return;
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

#[cfg(target_os = "linux")]
fn select_linux_ui_backend() -> Result<(), String> {
    use slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland as WaylandAttributes;
    use slint::winit_030::winit::platform::x11::WindowAttributesExtX11 as X11Attributes;

    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("WINIT_UNIX_BACKEND").as_deref() != Ok("x11");
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
