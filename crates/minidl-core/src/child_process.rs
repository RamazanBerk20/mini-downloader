//! Cross-platform setup for subprocesses that are part of the app's backend.

use std::process::Command;

/// Prevent a background console program from creating a terminal window.
///
/// Windows assigns a console host to console-subsystem executables such as
/// aria2c and yt-dlp unless they are launched with `CREATE_NO_WINDOW`. Other
/// platforms do not need special process flags.
pub fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // WinBase.h: CREATE_NO_WINDOW
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    let _ = command;
}
