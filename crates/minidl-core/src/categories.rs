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

/// One parsed match rule.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    /// Lowercased file extensions.
    Ext(Vec<String>),
    /// MIME prefixes (`"video/"` matches `video/mp4`).
    Mime(Vec<String>),
    /// Hostnames — exact, or suffix when written with a leading dot
    /// (`.example.com` matches `cdn.example.com`).
    Host(Vec<String>),
}

/// Parse a category's stored `rules` JSON. Two shapes are accepted:
/// the legacy flat array `["mp4","mkv"]` (one `Ext` rule) and the object list
/// `[{"match":"ext"|"mime"|"host","values":[...]}]`.
pub fn parse_rules(rules: &str) -> Vec<Rule> {
    if let Ok(values) = serde_json::from_str::<Vec<String>>(rules) {
        return if values.is_empty() { Vec::new() } else { vec![Rule::Ext(values)] };
    }
    #[derive(serde::Deserialize)]
    struct RuleObj {
        #[serde(rename = "match")]
        kind: String,
        #[serde(default)]
        values: Vec<String>,
    }
    serde_json::from_str::<Vec<RuleObj>>(rules)
        .map(|objs| {
            objs.into_iter()
                .filter(|o| !o.values.is_empty())
                .filter_map(|o| match o.kind.as_str() {
                    "ext" => Some(Rule::Ext(o.values)),
                    "mime" => Some(Rule::Mime(o.values)),
                    "host" => Some(Rule::Host(o.values)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reduce a URL authority to the bare hostname: drop userinfo, the port, and
/// IPv6 brackets, so `user@nas.example.com:8080` compares as `nas.example.com`.
fn normalize_host(host: &str) -> String {
    let h = host.rsplit_once('@').map(|(_, rest)| rest).unwrap_or(host);
    let h = if let Some(rest) = h.strip_prefix('[') {
        rest.split_once(']').map(|(addr, _)| addr).unwrap_or(rest)
    } else {
        // A bare colon-suffix is a port (IPv6 literals are always bracketed in URLs).
        h.rsplit_once(':').map(|(name, _)| name).filter(|n| !n.is_empty()).unwrap_or(h)
    };
    h.to_lowercase()
}

fn host_matches(host: &str, pattern: &str) -> bool {
    let host = normalize_host(host);
    let p = pattern.to_lowercase();
    if let Some(suffix) = p.strip_prefix('.') {
        host == suffix || host.ends_with(&p)
    } else {
        host == p || host.ends_with(&format!(".{p}"))
    }
}

/// First category (they arrive priority-ordered from the DB) with a matching
/// rule: extension, MIME prefix, or source host.
pub fn classify<'a>(
    filename: &str,
    mime: Option<&str>,
    host: Option<&str>,
    categories: &'a [Category],
) -> Option<&'a Category> {
    let ext = ext_of(filename);
    for c in categories {
        for rule in parse_rules(&c.rules) {
            let hit = match &rule {
                Rule::Ext(values) => {
                    !ext.is_empty() && values.iter().any(|v| v.to_lowercase() == ext)
                }
                Rule::Mime(values) => mime.is_some_and(|m| {
                    let m = m.to_lowercase();
                    values.iter().any(|v| m.starts_with(&v.to_lowercase()))
                }),
                Rule::Host(values) => {
                    host.is_some_and(|h| values.iter().any(|v| host_matches(h, v)))
                }
            };
            if hit {
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
        assert_eq!(classify("clip.mp4", None, None, &cats).unwrap().name, "Video");
        assert_eq!(classify("a.tar.gz", None, None, &cats).unwrap().name, "Archives");
        assert!(classify("readme", None, None, &cats).is_none());
    }

    #[test]
    fn classify_by_mime_prefix() {
        let cats = vec![cat("Video", r#"[{"match":"mime","values":["video/"]}]"#)];
        assert_eq!(classify("noext", Some("video/mp4"), None, &cats).unwrap().name, "Video");
        assert!(classify("noext", Some("audio/mpeg"), None, &cats).is_none());
        assert!(classify("noext", None, None, &cats).is_none());
    }

    #[test]
    fn classify_by_host_suffix() {
        let cats = vec![cat("Work", r#"[{"match":"host","values":[".example.com","files.org"]}]"#)];
        assert_eq!(classify("x", None, Some("cdn.example.com"), &cats).unwrap().name, "Work");
        assert_eq!(classify("x", None, Some("example.com"), &cats).unwrap().name, "Work");
        assert_eq!(classify("x", None, Some("files.org"), &cats).unwrap().name, "Work");
        assert_eq!(classify("x", None, Some("dl.files.org"), &cats).unwrap().name, "Work");
        assert!(classify("x", None, Some("notexample.com"), &cats).is_none());
        // Ports and userinfo from the raw URL authority must not break matching.
        assert_eq!(classify("x", None, Some("nas.example.com:8080"), &cats).unwrap().name, "Work");
        assert_eq!(classify("x", None, Some("user@files.org:21"), &cats).unwrap().name, "Work");
    }

    #[test]
    fn mixed_rules_and_legacy_shape() {
        let cats = vec![cat(
            "Media",
            r#"[{"match":"ext","values":["mp4"]},{"match":"mime","values":["video/"]}]"#,
        )];
        assert_eq!(classify("a.mp4", None, None, &cats).unwrap().name, "Media");
        assert_eq!(classify("b", Some("video/webm"), None, &cats).unwrap().name, "Media");
        // Legacy flat array still parses as one Ext rule.
        assert_eq!(parse_rules(r#"["mp4"]"#), vec![Rule::Ext(vec!["mp4".into()])]);
        assert!(parse_rules("[]").is_empty());
        assert!(parse_rules("not json").is_empty());
    }

    #[test]
    fn priority_order_is_caller_provided() {
        // list_categories orders by priority — classify takes the first hit.
        let cats = vec![
            cat("First", r#"["mp4"]"#),
            cat("Second", r#"["mp4"]"#),
        ];
        assert_eq!(classify("a.mp4", None, None, &cats).unwrap().name, "First");
    }
}
