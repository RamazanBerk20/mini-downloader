//! JDownloader-style link grabbing: extract + dedup links from pasted text.
//! Hand-rolled (no regex dependency) — splits on delimiters and matches schemes.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedLink {
    pub url: String,
    pub kind: String,
    pub host: String,
}

fn split_candidates(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || "\"'<>()[]{}|`\\".contains(c))
        .filter(|s| !s.is_empty())
}

fn host_of(url: &str) -> String {
    url.splitn(2, "://")
        .nth(1)
        .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or("").to_string())
        .unwrap_or_default()
}

fn classify(token: &str) -> Option<ParsedLink> {
    // Trim trailing punctuation often glued to URLs in prose.
    let t = token.trim_end_matches(|c: char| ".,;:!?\"'>)]".contains(c));
    let lower = t.to_ascii_lowercase();
    let kind = if lower.starts_with("magnet:?") {
        "magnet"
    } else if lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("ftp://")
    {
        if lower.ends_with(".torrent") {
            "torrent"
        } else {
            "http"
        }
    } else {
        return None;
    };
    Some(ParsedLink {
        url: t.to_string(),
        kind: kind.into(),
        host: host_of(t),
    })
}

/// Extract + dedup links from a blob of text (list, HTML, prose).
pub fn parse_links(text: &str) -> Vec<ParsedLink> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tok in split_candidates(text) {
        if let Some(link) = classify(tok) {
            if seen.insert(link.url.clone()) {
                out.push(link);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_dedups() {
        let text = r#"
            Get it at https://example.com/a.zip and https://example.com/a.zip (dup).
            Torrent: https://tracker.org/x.torrent
            magnet:?xt=urn:btih:ABC123&dn=thing
            <a href="http://site.net/file.bin">link</a>
            not a link: foo.bar
        "#;
        let links = parse_links(text);
        let urls: Vec<_> = links.iter().map(|l| l.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/a.zip"));
        assert_eq!(urls.iter().filter(|u| **u == "https://example.com/a.zip").count(), 1);
        assert!(urls.contains(&"https://tracker.org/x.torrent"));
        assert!(urls.iter().any(|u| u.starts_with("magnet:?xt=urn:btih:ABC123")));
        assert!(urls.contains(&"http://site.net/file.bin"));
        assert!(!urls.iter().any(|u| u.contains("foo.bar")));

        let torrent = links.iter().find(|l| l.url.ends_with(".torrent")).unwrap();
        assert_eq!(torrent.kind, "torrent");
        assert_eq!(links.iter().find(|l| l.kind == "magnet").is_some(), true);
        assert_eq!(host_of("https://example.com/a.zip"), "example.com");
    }
}
