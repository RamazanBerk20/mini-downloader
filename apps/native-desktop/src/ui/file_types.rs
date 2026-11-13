//! The file-type tile on a download row: one fixed, built-in category per
//! row, independent of the user-editable download categories.

use minidl_app::controller::DownloadView;
use minidl_core::model::DownloadStatus;

/// Categories the UI draws a tile for. `resolving` means "no name yet".
#[cfg(test)]
pub(crate) const CATEGORIES: &[&str] = &[
    "archive",
    "video",
    "stream",
    "audio",
    "image",
    "document",
    "pdf",
    "spreadsheet",
    "presentation",
    "code",
    "executable",
    "package",
    "disk-image",
    "torrent",
    "folder",
    "font",
    "ebook",
    "subtitle",
    "data",
    "generic",
    "resolving",
];

/// Locale key of a category's spoken name (the tile is never the only
/// carrier of the type).
pub(crate) fn label_key(category: &str) -> &'static str {
    match category {
        "archive" => "ftArchive",
        "video" => "ftVideo",
        "stream" => "ftStream",
        "audio" => "ftAudio",
        "image" => "ftImage",
        "document" => "ftDocument",
        "pdf" => "ftPdf",
        "spreadsheet" => "ftSpreadsheet",
        "presentation" => "ftPresentation",
        "code" => "ftCode",
        "executable" => "ftExecutable",
        "package" => "ftPackage",
        "disk-image" => "ftDiskImage",
        "torrent" => "ftTorrent",
        "folder" => "ftFolder",
        "font" => "ftFont",
        "ebook" => "ftEbook",
        "subtitle" => "ftSubtitle",
        "data" => "ftData",
        "resolving" => "ftResolving",
        _ => "ftGeneric",
    }
}

/// Category of a file name by its last extension, lowercase. The extension is
/// read from the raw name, so a right-to-left override that makes
/// `invoice\u{202E}fdp.exe` *look* like a PDF still yields `executable`.
pub(crate) fn by_extension(name: &str) -> Option<&'static str> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (_, extension) = name.rsplit_once('.')?;
    let extension = extension.trim().to_ascii_lowercase();
    Some(match extension.as_str() {
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "tbz" | "tbz2" | "xz" | "txz"
        | "zst" | "tzst" | "lz" | "lzma" | "lz4" | "cab" | "arj" | "z" => "archive",
        "iso" | "img" | "dmg" | "vhd" | "vhdx" | "qcow2" | "vmdk" | "vdi" => "disk-image",
        "deb" | "rpm" | "appimage" | "apk" | "aab" | "msi" | "msix" | "msixbundle" | "appx"
        | "flatpak" | "flatpakref" | "snap" | "pkg" | "xbps" => "package",
        "exe" | "com" | "scr" | "bat" | "cmd" | "ps1" | "vbs" | "sh" | "run" | "bin" | "jar"
        | "app" | "elf" => "executable",
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "flv" | "ts" | "m2ts" | "mts" | "wmv"
        | "mpg" | "mpeg" | "3gp" | "ogv" => "video",
        "mp3" | "flac" | "m4a" | "opus" | "aac" | "wav" | "ogg" | "oga" | "wma" | "alac"
        | "aif" | "aiff" | "mka" | "mid" | "midi" | "ape" => "audio",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" | "heic" | "heif" | "bmp"
        | "tif" | "tiff" | "ico" | "raw" | "cr2" | "nef" | "dng" | "psd" | "xcf" | "jxl" => "image",
        "pdf" => "pdf",
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "tex" | "pages" | "wpd" => "document",
        "xls" | "xlsx" | "xlsm" | "ods" | "csv" | "tsv" | "numbers" => "spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "presentation",
        "epub" | "mobi" | "azw" | "azw3" | "fb2" | "djvu" | "cbz" | "cbr" => "ebook",
        "html" | "htm" | "css" | "js" | "mjs" | "py" | "rs" | "c" | "cc" | "cpp" | "h" | "hpp"
        | "go" | "java" | "kt" | "rb" | "php" | "swift" | "lua" | "ipynb" | "patch" | "diff" => {
            "code"
        }
        "json" | "ndjson" | "xml" | "yaml" | "yml" | "toml" | "ini" | "sql" | "sqlite" | "db"
        | "parquet" | "bak" => "data",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "srt" | "vtt" | "ass" | "ssa" | "sub" | "sbv" => "subtitle",
        "torrent" | "meta4" | "metalink" => "torrent",
        _ => return None,
    })
}

/// Category from a server-reported MIME type, the last resort.
pub(crate) fn by_mime(mime: &str) -> Option<&'static str> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let exact = match mime.as_str() {
        "application/pdf" => Some("pdf"),
        "application/zip"
        | "application/x-zip-compressed"
        | "application/x-7z-compressed"
        | "application/vnd.rar"
        | "application/x-rar-compressed"
        | "application/gzip"
        | "application/x-gzip"
        | "application/x-tar"
        | "application/x-xz"
        | "application/x-bzip2"
        | "application/zstd" => Some("archive"),
        "application/x-msdownload"
        | "application/x-msdos-program"
        | "application/vnd.microsoft.portable-executable"
        | "application/x-executable"
        | "application/x-sh" => Some("executable"),
        "application/vnd.debian.binary-package"
        | "application/x-rpm"
        | "application/x-redhat-package-manager"
        | "application/vnd.android.package-archive"
        | "application/x-msi"
        | "application/x-ms-installer" => Some("package"),
        "application/x-iso9660-image" | "application/x-apple-diskimage" => Some("disk-image"),
        "application/x-bittorrent" | "application/metalink4+xml" => Some("torrent"),
        "application/epub+zip" | "application/x-mobipocket-ebook" => Some("ebook"),
        "application/json" | "application/xml" | "text/xml" | "application/sql" => Some("data"),
        "text/csv" | "application/vnd.ms-excel" => Some("spreadsheet"),
        "application/vnd.ms-powerpoint" => Some("presentation"),
        "application/msword" | "application/rtf" | "text/plain" | "text/markdown" => {
            Some("document")
        }
        "text/html" | "text/css" | "text/javascript" | "application/javascript" => Some("code"),
        "text/vtt" | "application/x-subrip" => Some("subtitle"),
        _ => None,
    };
    exact.or_else(|| {
        let (kind, subtype) = mime.split_once('/')?;
        Some(match kind {
            "video" => "video",
            "audio" => "audio",
            "image" => "image",
            "font" => "font",
            _ if subtype.contains("spreadsheet") => "spreadsheet",
            _ if subtype.contains("presentation") => "presentation",
            _ if subtype.contains("wordprocessing") || subtype.contains("opendocument.text") => {
                "document"
            }
            _ => return None,
        })
    })
}

/// The last path segment of a URL, query and fragment removed. Percent
/// escapes are left alone: an extension is plain ASCII in practice.
fn url_name(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next()?;
    let (_, path) = path.split_once('/')?;
    path.rsplit('/').next().filter(|name| !name.is_empty())
}

/// The tile category of a download row: the stored file name first, then
/// the URL's last path segment while the name is unknown, then the MIME type.
pub(crate) fn category(view: &DownloadView) -> &'static str {
    if matches!(view.kind.as_str(), "video" | "hls" | "dash") {
        let audio_only = view
            .media_opts
            .as_deref()
            .and_then(|options| serde_json::from_str::<minidl_ipc::MediaOptions>(options).ok())
            .is_some_and(|options| options.audio_only);
        return if audio_only { "audio" } else { "stream" };
    }
    let finished = view.status == DownloadStatus::Complete;
    if matches!(view.kind.as_str(), "torrent" | "magnet") {
        // A finished multi-file torrent is its root folder; a single-file
        // one is that file.
        return match (&view.filename, finished, view.is_dir) {
            (_, true, true) => "folder",
            (Some(name), true, false) => by_extension(name).unwrap_or("generic"),
            _ => "torrent",
        };
    }
    if let Some(name) = view
        .filename
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        return by_extension(name)
            .or_else(|| view.mime.as_deref().and_then(by_mime))
            .unwrap_or("generic");
    }
    url_name(&view.url)
        .and_then(by_extension)
        .or_else(|| view.mime.as_deref().and_then(by_mime))
        .unwrap_or(if finished { "generic" } else { "resolving" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(kind: &str, url: &str, filename: Option<&str>, mime: Option<&str>) -> DownloadView {
        let mut view: DownloadView = serde_json::from_value(serde_json::json!({
            "id": 1, "gid": null, "url": url, "filename": filename, "dir": "/dl",
            "status": "active", "kind": kind, "total_bytes": 0, "completed_bytes": 0,
            "download_speed": 0, "upload_speed": 0, "connections": 0, "num_seeders": 0,
            "referrer": null, "info_hash": null, "error_code": null, "error_message": null,
            "category_id": null, "created_at": 0, "finished_at": null, "page_url": null,
            "format_id": null, "speed_limit": null, "package_id": null, "mime": mime,
            "checksum": null, "start_at": null, "media_opts": null
        }))
        .unwrap();
        view.mime = mime.map(str::to_owned);
        view
    }

    #[test]
    fn extensions_map_to_their_categories() {
        for (name, expected) in [
            ("a.ZIP", "archive"),
            ("linux.tar.gz", "archive"),
            ("b.tar.zst", "archive"),
            ("ubuntu.iso", "disk-image"),
            ("app.AppImage", "package"),
            ("tool.deb", "package"),
            ("setup.msi", "package"),
            ("setup.exe", "executable"),
            ("movie.mkv", "video"),
            ("song.flac", "audio"),
            ("photo.jpeg", "image"),
            ("paper.pdf", "pdf"),
            ("notes.docx", "document"),
            ("sheet.xlsx", "spreadsheet"),
            ("talk.pptx", "presentation"),
            ("main.rs", "code"),
            ("dump.sql", "data"),
            ("font.woff2", "font"),
            ("book.epub", "ebook"),
            ("film.srt", "subtitle"),
            ("x.torrent", "torrent"),
            ("x.meta4", "torrent"),
        ] {
            assert_eq!(by_extension(name), Some(expected), "{name}");
        }
        assert_eq!(by_extension("README"), None);
        assert_eq!(by_extension("weird.qqq"), None);
    }

    #[test]
    fn a_bidi_override_cannot_disguise_the_real_extension() {
        assert_eq!(by_extension("invoice\u{202E}fdp.exe"), Some("executable"));
        let row = view("http", "https://x/y", Some("invoice\u{202E}fdp.exe"), None);
        assert_eq!(category(&row), "executable");
    }

    #[test]
    fn mime_types_are_the_last_resort() {
        assert_eq!(by_mime("video/mp4"), Some("video"));
        assert_eq!(by_mime("Application/PDF; charset=binary"), Some("pdf"));
        assert_eq!(by_mime("application/x-7z-compressed"), Some("archive"));
        assert_eq!(
            by_mime("application/vnd.debian.binary-package"),
            Some("package")
        );
        assert_eq!(
            by_mime("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            Some("spreadsheet")
        );
        assert_eq!(by_mime("application/octet-stream"), None);
    }

    #[test]
    fn rows_use_name_then_url_then_mime() {
        let named = view("http", "https://x/a.zip", Some("b.pdf"), Some("video/mp4"));
        assert_eq!(category(&named), "pdf");
        let from_url = view("http", "https://x/dist/a.iso?token=1#f", None, None);
        assert_eq!(category(&from_url), "disk-image");
        let from_mime = view("http", "https://x/download?id=3", None, Some("audio/ogg"));
        assert_eq!(category(&from_mime), "audio");
        let unknown = view("http", "https://x/download?id=3", None, None);
        assert_eq!(category(&unknown), "resolving");
        let mut done = unknown.clone();
        done.status = DownloadStatus::Complete;
        assert_eq!(category(&done), "generic");
    }

    #[test]
    fn torrents_videos_and_folders_keep_their_own_tiles() {
        assert_eq!(
            category(&view("magnet", "magnet:?xt=urn:btih:a", None, None)),
            "torrent"
        );
        let mut folder = view("torrent", "file:a.torrent", Some("Album"), None);
        folder.status = DownloadStatus::Complete;
        folder.is_dir = true;
        assert_eq!(category(&folder), "folder");
        folder.is_dir = false;
        folder.filename = Some("one.mkv".into());
        assert_eq!(category(&folder), "video");
        assert_eq!(
            category(&view("video", "https://x/watch?v=1", None, None)),
            "stream"
        );
        let mut audio = view("video", "https://x/watch?v=1", None, None);
        audio.media_opts = Some(r#"{"audioOnly":true}"#.into());
        assert_eq!(category(&audio), "audio");
        for category in CATEGORIES {
            assert!(label_key(category).starts_with("ft"));
        }
    }

    #[test]
    fn every_category_has_a_glyph_and_a_tint() {
        let kit = include_str!("../../ui/ui-kit.slint");
        for category in CATEGORIES {
            assert!(
                kit.contains(&format!("\"ft-{category}\"")),
                "glyph for {category}"
            );
            if !matches!(*category, "generic" | "resolving") {
                assert!(
                    kit.contains(&format!("type == \"{category}\"")),
                    "tint for {category}"
                );
            }
        }
    }
}
