//! End-to-end engine tests against a real aria2c and a deterministic loopback
//! HTTP fixture. Ignored by default because `aria2c` must be on PATH.
//!
//! Run with:
//!   cargo test -p minidl-core --test engine_e2e -- --ignored --nocapture

use std::time::Duration;

use minidl_core::aria2::{
    build_add_options, Aria2Event, Engine, EngineDefaults, LaunchOptions, STATUS_KEYS,
};
use minidl_core::ipc::{CaptureJob, DownloadKind};

#[path = "../../../test-support/loopback_http.rs"]
mod loopback_http;

use loopback_http::LoopbackHttp;

fn job(url: &str, filename: &str) -> CaptureJob {
    CaptureJob {
        url: url.into(),
        filename: Some(filename.into()),
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
        batch_id: None,
        batch_name: None,
    }
}

#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn download_completes_with_notification() {
    let fixture = LoopbackHttp::start(512 * 1_024).await;
    let root = tempfile::tempdir().expect("temporary e2e directory");
    let downloads = root.path().join("downloads");
    let data = root.path().join("data");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&data).unwrap();

    let engine = Engine::launch(LaunchOptions {
        aria2c_path: None,
        download_dir: downloads.clone(),
        data_dir: data,
        max_concurrent: 2,
        ..Default::default()
    })
    .await
    .expect("engine launch");
    assert!(engine.rpc.get_version().await.is_ok());

    let mut events = engine.subscribe();
    let capture = job(&fixture.url, "e2e.bin");
    let options = build_add_options(
        &capture,
        downloads.to_str().unwrap(),
        &EngineDefaults {
            split: 1,
            max_connection_per_server: 1,
            ..Default::default()
        },
    );
    let gid = engine
        .rpc
        .add_uri(
            std::slice::from_ref(&capture.url),
            serde_json::Value::Object(options),
        )
        .await
        .expect("add loopback URI");

    let completed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            tokio::select! {
                event = events.recv() => {
                    if let Ok(Aria2Event::Complete(completed_gid)) = event {
                        if completed_gid == gid { break true; }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let status = engine.rpc.tell_status(&gid, STATUS_KEYS).await.unwrap();
                    if status.get("status").and_then(|value| value.as_str()) == Some("complete") {
                        break true;
                    }
                }
            }
        }
    })
    .await
    .unwrap_or(false);

    engine.shutdown().await;
    assert!(completed, "loopback download did not complete in time");
    assert_eq!(
        std::fs::read(downloads.join("e2e.bin")).unwrap(),
        fixture.payload.as_slice()
    );
    fixture.shutdown();
}

/// A saved session must be inert at cold startup, then resume normally after
/// an explicit user action.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn restored_session_stays_paused_until_explicit_resume() {
    const GID: &str = "0000000000000001";

    let fixture = LoopbackHttp::start(384 * 1_024).await;
    let root = tempfile::tempdir().expect("temporary e2e directory");
    let downloads = root.path().join("downloads");
    let data = root.path().join("data");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("aria2.session"),
        format!("{}\n gid={GID}\n out=restored.bin\n", fixture.url),
    )
    .unwrap();

    let engine = Engine::launch(LaunchOptions {
        aria2c_path: None,
        download_dir: downloads.clone(),
        data_dir: data,
        max_concurrent: 2,
        ..Default::default()
    })
    .await
    .expect("engine launch");

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!downloads.join("restored.bin").exists());
    assert!(!downloads.join("restored.bin.aria2").exists());

    engine.rpc.unpause(GID).await.expect("explicit resume");
    let completed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = engine
                .rpc
                .tell_status(GID, STATUS_KEYS)
                .await
                .unwrap_or_default();
            if status.get("status").and_then(|value| value.as_str()) == Some("complete") {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(false);

    engine.shutdown().await;
    assert!(completed, "explicitly resumed session did not complete");
    assert_eq!(
        std::fs::read(downloads.join("restored.bin")).unwrap(),
        fixture.payload.as_slice()
    );
    fixture.shutdown();
}
