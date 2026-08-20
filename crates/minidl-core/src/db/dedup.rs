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

fn is_transient_query_key(key: &str) -> bool {
    is_strong_transient_query_key(key)
        || matches!(
            key.to_ascii_lowercase().as_str(),
            // Companion fields used by common CDN signatures. These are only
            // stripped when the URL also contains a strong signature field.
            "h" | "hash"
                | "key"
                | "key-pair-id"
                | "user_id"
                | "sv"
                | "spr"
                | "st"
                | "se"
                | "sr"
                | "sp"
        )
}

/// Return the stable resource URL for a fully signed CDN query. Ordinary query
/// strings are deliberately left alone: `download?id=1` and `download?id=2`
/// can be different files. A query is stripped only when every field belongs
/// to a known signing scheme and at least one high-confidence signature field
/// is present.
pub(super) fn signed_source_base(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let keys = parsed
        .query_pairs()
        .map(|(key, _)| key.into_owned())
        .collect::<Vec<_>>();
    if keys.is_empty()
        || !keys.iter().any(|key| is_strong_transient_query_key(key))
        || !keys.iter().all(|key| is_transient_query_key(key))
    {
        return None;
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn unsigned_source(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    if parsed.query().is_some() {
        return None;
    }
    parsed.set_fragment(None);
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
