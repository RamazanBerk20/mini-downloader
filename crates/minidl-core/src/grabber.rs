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
    // NB: `[]{}` are deliberately NOT delimiters — they carry numeric range
    // patterns (`img[01-50].jpg`, `z{1..9}.bin`) expanded in `parse_links`.
    text.split(|c: char| c.is_whitespace() || "\"'<>()|`\\".contains(c))
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

/// Max URLs a single `[a-b]` / `{a..b}` pattern may expand to (runaway guard).
const MAX_EXPAND: u64 = 1000;

/// Expand one numeric range pattern in a token: `file[001-050].jpg` or
/// `file{1..50}.jpg`. Leading zeros in the bound set zero-padding width. A
/// missing/oversized/invalid pattern yields the token unchanged.
fn expand_pattern(token: &str) -> Vec<String> {
    let try_range = |open: char, close: char, sep: &str| -> Option<Vec<String>> {
        let lo = token.find(open)?;
        let hi = token[lo..].find(close)? + lo;
        let inner = &token[lo + 1..hi];
        let (a, b) = inner.split_once(sep)?;
        let (start, end) = (a.parse::<u64>().ok()?, b.parse::<u64>().ok()?);
        let (from, to) = if start <= end { (start, end) } else { (end, start) };
        if to - from + 1 > MAX_EXPAND {
            return None;
        }
        let pad = if a.starts_with('0') || b.starts_with('0') {
            a.len().max(b.len())
        } else {
            0
        };
        let (prefix, suffix) = (&token[..lo], &token[hi + 1..]);
        Some((from..=to).map(|n| format!("{prefix}{n:0>pad$}{suffix}")).collect())
    };
    try_range('[', ']', "-")
        .or_else(|| try_range('{', '}', ".."))
        .unwrap_or_else(|| vec![token.to_string()])
}

/// Extract + dedup links from a blob of text (list, HTML, prose). Numeric range
/// patterns (`[001-200]`, `{1..50}`) are expanded before classification.
pub fn parse_links(text: &str) -> Vec<ParsedLink> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tok in split_candidates(text) {
        for expanded in expand_pattern(tok) {
            if let Some(link) = classify(&expanded) {
                if seen.insert(link.url.clone()) {
                    out.push(link);
                }
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

    #[test]
    fn expands_numeric_patterns() {
        let links = parse_links("https://x.com/img[01-03].jpg");
        let urls: Vec<_> = links.iter().map(|l| l.url.clone()).collect();
        assert_eq!(links.len(), 3);
        assert!(urls.contains(&"https://x.com/img01.jpg".to_string()));
        assert!(urls.contains(&"https://x.com/img03.jpg".to_string()));

        // Brace form, no leading zero → no padding.
        let l2 = parse_links("http://y/z{8..10}.bin");
        assert_eq!(l2.len(), 3);
        assert!(l2.iter().any(|l| l.url == "http://y/z9.bin"));

        // A runaway range is left unexpanded (single literal token, unmatched).
        let l3 = parse_links("https://x/[1-100000].bin");
        assert!(l3.iter().all(|l| l.url.contains('[')));
    }
}
