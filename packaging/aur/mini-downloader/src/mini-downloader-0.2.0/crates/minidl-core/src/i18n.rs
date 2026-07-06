//! Minimal localization for the few user-facing strings the engine layer emits
//! itself (desktop notifications). The full UI catalog lives in the frontend;
//! this only covers what Rust surfaces directly. Falls back to English.

/// A localizable message emitted from the engine / sync layer.
#[derive(Clone, Copy, Debug)]
pub enum Msg {
    DownloadComplete,
    DownloadFailed,
}

/// Translate `msg` for a locale tag (e.g. `"tr"`, `"zh-CN"`); unknown → English.
pub fn tr(locale: &str, msg: Msg) -> &'static str {
    let lang = locale
        .split(|c| c == '-' || c == '_')
        .next()
        .unwrap_or("en");
    match msg {
        Msg::DownloadComplete => match lang {
            "tr" => "İndirme tamamlandı",
            "es" => "Descarga completada",
            "fr" => "Téléchargement terminé",
            "de" => "Download abgeschlossen",
            "ru" => "Загрузка завершена",
            "ar" => "اكتمل التنزيل",
            "zh" => "下载完成",
            "ja" => "ダウンロード完了",
            "ko" => "다운로드 완료",
            _ => "Download complete",
        },
        Msg::DownloadFailed => match lang {
            "tr" => "İndirme başarısız",
            "es" => "Descarga fallida",
            "fr" => "Échec du téléchargement",
            "de" => "Download fehlgeschlagen",
            "ru" => "Ошибка загрузки",
            "ar" => "فشل التنزيل",
            "zh" => "下载失败",
            "ja" => "ダウンロード失敗",
            "ko" => "다운로드 실패",
            _ => "Download failed",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_and_maps_lang() {
        assert_eq!(tr("en", Msg::DownloadComplete), "Download complete");
        assert_eq!(tr("tr", Msg::DownloadComplete), "İndirme tamamlandı");
        assert_eq!(tr("zh-CN", Msg::DownloadFailed), "下载失败");
        assert_eq!(tr("xx", Msg::DownloadFailed), "Download failed");
    }
}
