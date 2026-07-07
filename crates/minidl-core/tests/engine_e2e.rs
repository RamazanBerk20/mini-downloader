//! End-to-end engine test against a real aria2c. Ignored by default (needs
//! network + `aria2c` on PATH). Run with:
//!   cargo test -p minidl-core --test engine_e2e -- --ignored --nocapture

use std::time::Duration;

use minidl_core::aria2::{
    build_add_options, Aria2Event, Engine, EngineDefaults, LaunchOptions, STATUS_KEYS,
};
use minidl_core::ipc::{CaptureJob, DownloadKind};

fn job(url: &str) -> CaptureJob {
    CaptureJob {
        url: url.into(),
        filename: Some("e2e.bin".into()),
        referrer: None,
        user_agent: None,
        cookie: None,
        extra_headers: vec![],
        kind: DownloadKind::Http,
        mime: None,
        size: None,
        page_url: None,
        cookie_store_id: None,
        torrent_b64: None,
    }
}

#[tokio::test]
#[ignore]
async fn download_completes_with_notification() {
    let dir = std::env::temp_dir().join(format!("ldm-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let engine = Engine::launch(LaunchOptions {
        aria2c_path: None,
        download_dir: dir.clone(),
        data_dir: dir.clone(),
        max_concurrent: 5,
        ..Default::default()
    })
    .await
    .expect("engine launch");

    assert!(engine.rpc.get_version().await.is_ok());

    let mut events = engine.subscribe();

    let j = job("https://speed.cloudflare.com/__down?bytes=1048576");
    let opts = build_add_options(&j, dir.to_str().unwrap(), &EngineDefaults::default());
    let gid = engine
        .rpc
        .add_uri(&[j.url.clone()], serde_json::Value::Object(opts))
        .await
        .expect("addUri");

    // Prefer the WebSocket completion notification; fall back to polling.
    let completed = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            tokio::select! {
                ev = events.recv() => {
                    if let Ok(Aria2Event::Complete(g)) = ev {
                        if g == gid { break true; }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    let st = engine.rpc.tell_status(&gid, STATUS_KEYS).await.unwrap();
                    if st.get("status").and_then(|s| s.as_str()) == Some("complete") { break true; }
                }
            }
        }
    })
    .await
    .unwrap_or(false);

    engine.shutdown().await;

    assert!(completed, "download did not complete in time");
    let file = dir.join("e2e.bin");
    assert!(file.exists(), "file missing: {}", file.display());
    let size = std::fs::metadata(&file).unwrap().len();
    assert_eq!(size, 1_048_576, "unexpected size");

    let _ = std::fs::remove_dir_all(&dir);
}
