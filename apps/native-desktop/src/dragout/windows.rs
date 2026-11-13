//! Windows: an OLE drag of the shell's own data object for the files
//! (`CF_HDROP`, `Shell IDList Array`, file contents, drag image), copy only.
//! The decisions are in `shell_path` and `EFFECT`, compiled and tested on
//! every platform; the FFI below only runs them.

/// DROPEFFECT_COPY alone: a drop may copy a download, never move it away.
pub(super) const EFFECT: u32 = 1;

/// A path as `SHParseDisplayName` takes it: absolute, without the `\\?\`
/// verbatim prefix, NUL-terminated UTF-16. None for anything else.
pub(super) fn shell_path(path: &str) -> Option<Vec<u16>> {
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    };
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\";
    let unc = path.starts_with(r"\\") && path.len() > 2;
    if !(drive || unc) || path.contains('\0') {
        return None;
    }
    Some(path.encode_utf16().chain([0]).collect())
}

/// Run the modal shell drag; returns when the drop or cancel is done.
#[cfg(windows)]
pub(super) fn drag(hwnd: isize, paths: &[std::path::PathBuf]) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject};
    use windows::Win32::System::Ole::{IDropSource, DROPEFFECT};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName,
    };

    let wide = paths
        .iter()
        .filter_map(|path| shell_path(&path.to_string_lossy()))
        .collect::<Vec<_>>();
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
    for name in &wide {
        let mut pidl = std::ptr::null_mut();
        // Safety: `name` is NUL-terminated and outlives the call.
        if unsafe {
            SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None)
        }
        .is_ok()
        {
            pidls.push(pidl);
        }
    }
    if !pidls.is_empty() {
        let list = pidls
            .iter()
            .map(|pidl| pidl.cast_const())
            .collect::<Vec<_>>();
        // Safety: every PIDL is live until freed below.
        let _ = unsafe {
            SHCreateShellItemArrayFromIDLists(&list).and_then(|items| {
                let data: IDataObject = items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)?;
                SHDoDragDrop(
                    Some(HWND(hwnd as _)),
                    &data,
                    None::<&IDropSource>,
                    DROPEFFECT(EFFECT),
                )
            })
        };
    }
    for pidl in pidls {
        // Safety: allocated by SHParseDisplayName, freed once.
        unsafe { CoTaskMemFree(Some(pidl.cast_const().cast())) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(wide: Option<Vec<u16>>) -> Option<String> {
        let wide = wide?;
        assert_eq!(wide.last(), Some(&0), "NUL-terminated");
        Some(String::from_utf16(&wide[..wide.len() - 1]).unwrap())
    }

    #[test]
    fn shell_paths_are_absolute_plain_and_terminated() {
        assert_eq!(
            text(shell_path(r"C:\Users\me\Downloads\a b #1%.iso")).as_deref(),
            Some(r"C:\Users\me\Downloads\a b #1%.iso")
        );
        assert_eq!(
            text(shell_path(r"\\?\D:\çay\ünï.mkv")).as_deref(),
            Some(r"D:\çay\ünï.mkv")
        );
        assert_eq!(
            text(shell_path(r"\\?\UNC\nas\share\f.zip")).as_deref(),
            Some(r"\\nas\share\f.zip")
        );
        assert_eq!(
            text(shell_path(r"\\nas\share\f.zip")).as_deref(),
            Some(r"\\nas\share\f.zip")
        );
        assert_eq!(shell_path(r"relative\f.zip"), None);
        assert_eq!(shell_path("C:"), None);
        assert_eq!(shell_path(r"C:\a\0b".replace(r"\0", "\0").as_str()), None);
        assert_eq!(shell_path(r"\\"), None);
        assert_eq!(
            EFFECT, 1,
            "DROPEFFECT_COPY only, never MOVE (2) or LINK (4)"
        );
    }
}
