//! File classification + path expansion for category auto-organize.

use std::path::PathBuf;

use crate::model::Category;

/// Expand a category directory. A leading `~/Downloads`, `~/Videos`, … resolves
/// to the *localized* XDG user directory; other `~/` paths expand to `$HOME`.
pub fn expand(dir: &str) -> PathBuf {
    crate::paths::resolve_home_path(dir)
}

/// Lowercased file extension, or empty when there is none.
pub fn ext_of(filename: &str) -> String {
    match filename.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && !ext.contains('/') => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// First category whose rule list contains the file's extension.
pub fn classify<'a>(filename: &str, categories: &'a [Category]) -> Option<&'a Category> {
    let ext = ext_of(filename);
    if ext.is_empty() {
        return None;
    }
    for c in categories {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(&c.rules) {
            if values.iter().any(|v| v.to_lowercase() == ext) {
                return Some(c);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(name: &str, rules: &str) -> Category {
        Category { id: 1, name: name.into(), dir: format!("~/{name}"), rules: rules.into(), priority: 0 }
    }

    #[test]
    fn ext_handling() {
        assert_eq!(ext_of("a.tar.gz"), "gz");
        assert_eq!(ext_of("movie.MP4"), "mp4");
        assert_eq!(ext_of("noext"), "");
        assert_eq!(ext_of(""), "");
    }

    #[test]
    fn classify_by_extension() {
        let cats = vec![
            cat("Video", r#"["mkv","mp4"]"#),
            cat("Archives", r#"["zip","gz"]"#),
        ];
        assert_eq!(classify("clip.mp4", &cats).unwrap().name, "Video");
        assert_eq!(classify("a.tar.gz", &cats).unwrap().name, "Archives");
        assert!(classify("readme", &cats).is_none());
    }
}
