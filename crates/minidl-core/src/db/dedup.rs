/// Result of an idempotent download insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadInsertResult {
    Inserted(i64),
    Existing(i64),
}

impl DownloadInsertResult {
    pub fn id(self) -> i64 {
        match self {
            Self::Inserted(id) | Self::Existing(id) => id,
        }
    }
}

fn is_strong_transient_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("x-amz-")
        || key.starts_with("x-goog-")
        || matches!(
            key.as_str(),
            "expires"
                | "expiry"
                | "exp"
                | "signature"
                | "sig"
                | "token"
                | "access_token"
                | "auth"
                | "auth_key"
                | "md5"
                | "policy"
        )
}

/// Pure validity-window fields that rotate with every signature. Only these
/// and the signature fields above are ignored; everything else in the query
/// (`key`, `h`, `id`, `user_id`, …) may name the file and is compared.
fn is_timing_query_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "expires" | "exp" | "se" | "st"
    )
}

/// Return the stable resource URL for a signed CDN link: the URL without its
/// signature and expiry fields, with the remaining query pairs sorted.
/// Ordinary query strings are never touched: a URL without a high-confidence
/// signature field returns `None` and is compared exactly.
pub(super) fn signed_source_base(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if !pairs
        .iter()
        .any(|(key, _)| is_strong_transient_query_key(key))
    {
        return None;
    }
    let mut kept = pairs
        .into_iter()
        .filter(|(key, _)| !is_strong_transient_query_key(key) && !is_timing_query_key(key))
        .collect::<Vec<_>>();
    kept.sort();
    parsed.set_fragment(None);
    if kept.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(kept);
    }
    Some(parsed.to_string())
}

/// The part of a signed source before its query: the SQL prefilter for rows
/// that may match; [`duplicate_sources_match`] makes the decision.
pub(super) fn signed_source_path(source: &str) -> Option<String> {
    let base = signed_source_base(source)?;
    Some(base.split('?').next().unwrap_or_default().to_owned())
}

/// An unsigned source without its fragment and with sorted query pairs, so it
/// compares equal to the signature-free base of a signed link.
fn unsigned_source(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    parsed.set_fragment(None);
    let mut pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        pairs.sort();
        parsed.query_pairs_mut().clear().extend_pairs(pairs);
    }
    Some(parsed.to_string())
}

/// Compare download sources while tolerating short-lived CDN signatures.
/// This is public so the desktop ingestion layer can compare a target file's
/// recorded origin URL before deciding that it is already downloaded.
pub fn duplicate_sources_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (signed_source_base(left), signed_source_base(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(left), None) => unsigned_source(right).is_some_and(|right| left == right),
        (None, Some(right)) => unsigned_source(left).is_some_and(|left| left == right),
        (None, None) => false,
    }
}
