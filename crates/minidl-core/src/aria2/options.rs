//! Mapping from a captured job + engine defaults to aria2 `addUri` options.
//!
//! aria2 requires **every option value to be a string** (`"16"`, `"1M"`,
//! `"true"`) — a JSON number or bool is rejected. All builders here emit strings.

use minidl_ipc::CaptureJob;

use crate::files::safe_out_name;
use serde_json::{Map, Value};

/// Per-download tuning that maps to IDM-style "connections/segments".
#[derive(Debug, Clone)]
pub struct EngineDefaults {
    /// aria2 `split` (`-s`): max pieces a file is divided into.
    pub split: u32,
    /// aria2 `max-connection-per-server` (`-x`, hard cap 16): the real limit on
    /// parallel connections to one host.
    pub max_connection_per_server: u32,
    /// aria2 `min-split-size` (`-k`): a new connection only opens if the
    /// remaining chunk is at least this big. aria2's 20M default silently
    /// disables splitting on medium files, so we default much lower.
    pub min_split_size: String,
}

impl Default for EngineDefaults {
    fn default() -> Self {
        Self {
            split: 16,
            max_connection_per_server: 16,
            min_split_size: "1M".into(),
        }
    }
}

/// Build the aria2 options object for an `addUri` call from a captured job.
///
/// `dir` is where aria2 writes: the job's private staging folder (see
/// [`crate::files::stage_dir`]); the finished file is moved into place later.
/// Cookies/headers are replayed verbatim: dropping them turns a working browser
/// download into a 403. They are valid for the job's URL only, so a job that
/// carries them may not follow redirects (resolve them first with
/// [`crate::redirect::resolve`]).
///
/// No `continue`: with it aria2 appends to, or overwrites, any existing file of
/// the same name that has no control file. Resume works through the `.aria2`
/// control file alone.
pub fn build_add_options(
    job: &CaptureJob,
    dir: &str,
    defaults: &EngineDefaults,
) -> Map<String, Value> {
    let mut opts = Map::new();
    opts.insert("dir".into(), Value::String(dir.to_string()));
    if let Some(name) = &job.filename {
        if let Some(out) = safe_out_name(name) {
            opts.insert("out".into(), Value::String(out));
        }
    }
    if let Some(referer) = &job.referrer {
        opts.insert("referer".into(), Value::String(referer.clone()));
    }
    if let Some(ua) = &job.user_agent {
        opts.insert("user-agent".into(), Value::String(ua.clone()));
    }

    // Cookie + any extra headers go through the `header` array. (Referer/UA get
    // dedicated options above.)
    let mut headers: Vec<Value> = Vec::new();
    if let Some(cookie) = &job.cookie {
        headers.push(Value::String(format!("Cookie: {cookie}")));
    }
    for (name, value) in &job.extra_headers {
        headers.push(Value::String(format!("{name}: {value}")));
    }
    if !headers.is_empty() {
        opts.insert("header".into(), Value::Array(headers));
    }
    if crate::redirect::carries_credentials(job) {
        opts.insert("max-redirect".into(), Value::String("0".into()));
    }

    opts.insert("split".into(), Value::String(defaults.split.to_string()));
    opts.insert(
        "max-connection-per-server".into(),
        Value::String(defaults.max_connection_per_server.to_string()),
    );
    opts.insert(
        "min-split-size".into(),
        Value::String(defaults.min_split_size.clone()),
    );

    // Survive transient network flaps (Wi-Fi/DNS blips) instead of hard-erroring
    // a multi-hour download: aria2's defaults (retry-wait=0, max-tries=5) exhaust
    // in milliseconds. Unlimited retries, but give up after repeated 404s so a
    // genuinely dead URL still fails.
    opts.insert("max-tries".into(), Value::String("0".into()));
    opts.insert("retry-wait".into(), Value::String("10".into()));
    opts.insert("connect-timeout".into(), Value::String("30".into()));
    opts.insert("max-file-not-found".into(), Value::String("5".into()));
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidl_ipc::DownloadKind;

    fn job() -> CaptureJob {
        CaptureJob {
            url: "https://cdn.example/big.iso".into(),
            filename: Some("big.iso".into()),
            referrer: Some("https://example/page".into()),
            user_agent: Some("Mozilla/5.0".into()),
            cookie: Some("sid=abc".into()),
            extra_headers: vec![("Authorization".into(), "Bearer x".into())],
            kind: DownloadKind::Http,
            mime: None,
            size: Some(1_000_000),
            page_url: None,
            cookie_store_id: None,
            torrent_b64: None,
            batch_id: None,
            batch_name: None,
        }
    }

    #[test]
    fn all_values_are_strings() {
        let opts = build_add_options(&job(), "/home/u/Downloads", &EngineDefaults::default());
        for (k, v) in &opts {
            match v {
                Value::String(_) | Value::Array(_) => {}
                other => panic!("option {k} is not a string/array: {other:?}"),
            }
        }
    }

    #[test]
    fn traversal_filename_reduced_to_basename() {
        let mut j = job();
        j.filename = Some("../../../../.config/autostart/pwn.desktop".into());
        let opts = build_add_options(&j, "/dl", &EngineDefaults::default());
        assert_eq!(opts["out"], Value::String("pwn.desktop".into()));

        // A pure dot-segment / trailing separator yields no `out` (aria2 names it).
        j.filename = Some("../../".into());
        let opts = build_add_options(&j, "/dl", &EngineDefaults::default());
        assert!(!opts.contains_key("out"));

        // Backslash separator (Windows-style) is stripped too.
        j.filename = Some("..\\..\\evil.exe".into());
        let opts = build_add_options(&j, "/dl", &EngineDefaults::default());
        assert_eq!(opts["out"], Value::String("evil.exe".into()));

        // Windows-hostile names are reduced the same way on every platform.
        j.filename = Some("notes.txt:payload".into());
        let opts = build_add_options(&j, "/dl", &EngineDefaults::default());
        assert_eq!(opts["out"], Value::String("notes.txt_payload".into()));
    }

    #[test]
    fn anonymous_jobs_may_follow_redirects() {
        let mut j = job();
        j.cookie = None;
        j.extra_headers.clear();
        let opts = build_add_options(&j, "/dl", &EngineDefaults::default());
        assert!(!opts.contains_key("max-redirect"));
        assert!(!opts.contains_key("header"));
    }

    #[test]
    fn maps_core_fields() {
        let opts = build_add_options(&job(), "/dl", &EngineDefaults::default());
        assert_eq!(opts["dir"], Value::String("/dl".into()));
        assert_eq!(opts["out"], Value::String("big.iso".into()));
        assert_eq!(
            opts["referer"],
            Value::String("https://example/page".into())
        );
        assert_eq!(
            opts["max-connection-per-server"],
            Value::String("16".into())
        );
        // `continue` would overwrite a same-named file that has no control file.
        assert!(!opts.contains_key("continue"));
        // Credentials are origin-bound: aria2 may not take them across a redirect.
        assert_eq!(opts["max-redirect"], Value::String("0".into()));
        let headers = opts["header"].as_array().unwrap();
        assert!(headers.contains(&Value::String("Cookie: sid=abc".into())));
        assert!(headers.contains(&Value::String("Authorization: Bearer x".into())));
    }
}
