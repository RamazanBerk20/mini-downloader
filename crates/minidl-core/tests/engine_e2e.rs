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

async fn wait_for_status(engine: &Engine, gid: &str) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = engine
                .rpc
                .tell_status(gid, STATUS_KEYS)
                .await
                .unwrap_or_default();
            match status.get("status").and_then(|value| value.as_str()) {
                Some(done @ ("complete" | "error")) => break done.to_owned(),
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .ok()
}

/// Audit HIGH #1 reproduced against real aria2: a credentialed capture whose
/// URL redirects to another origin. aria2 used to replay `Cookie` and
/// `Authorization` to the redirect target.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn captured_credentials_never_reach_another_origin() {
    let target = LoopbackHttp::start(256 * 1_024).await;
    let origin = LoopbackHttp::redirect_to(&target.url).await;
    let root = tempfile::tempdir().expect("temporary e2e directory");
    let downloads = root.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let engine = Engine::launch(LaunchOptions {
        download_dir: downloads.clone(),
        data_dir: root.path().join("data"),
        ..Default::default()
    })
    .await
    .expect("engine launch");

    let mut capture = job(&origin.url, "secret.bin");
    capture.cookie = Some("sid=SECRET".into());
    capture.extra_headers = vec![("Authorization".into(), "Bearer TOKEN".into())];
    let capture = minidl_core::redirect::resolve(&capture, None, false)
        .await
        .expect("resolve redirects");
    let options = build_add_options(
        &capture,
        downloads.to_str().unwrap(),
        &EngineDefaults::default(),
    );
    let gid = engine
        .rpc
        .add_uri(
            std::slice::from_ref(&capture.url),
            serde_json::Value::Object(options),
        )
        .await
        .expect("add");
    let status = wait_for_status(&engine, &gid).await;
    engine.shutdown().await;

    assert!(
        origin.saw_header("cookie"),
        "the capture origin gets its own cookie"
    );
    assert!(
        !target.saw_header("cookie"),
        "cookie leaked to the redirect target"
    );
    assert!(
        !target.saw_header("authorization"),
        "authorization leaked to the redirect target"
    );
    assert_eq!(status.as_deref(), Some("complete"));
    assert_eq!(
        std::fs::read(downloads.join("secret.bin")).unwrap(),
        target.payload.as_slice()
    );
    origin.shutdown();
    target.shutdown();
}

/// Audit HIGH #2 reproduced against real aria2: a download whose name matches
/// a file the user already has. With `continue=true` aria2 overwrote it (or
/// spliced it with the remote tail).
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn an_existing_file_of_the_same_name_is_never_overwritten() {
    let fixture = LoopbackHttp::start(128 * 1_024).await;
    let root = tempfile::tempdir().expect("temporary e2e directory");
    let downloads = root.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(downloads.join("report.pdf"), b"the user's own report").unwrap();
    let engine = Engine::launch(LaunchOptions {
        download_dir: downloads.clone(),
        data_dir: root.path().join("data"),
        ..Default::default()
    })
    .await
    .expect("engine launch");

    let capture = job(&fixture.url, "report.pdf");
    let mut options = build_add_options(
        &capture,
        downloads.to_str().unwrap(),
        &EngineDefaults::default(),
    );
    // What ingest used to add when duplicate prevention is on.
    options.insert("auto-file-renaming".into(), "false".into());
    let gid = engine
        .rpc
        .add_uri(
            std::slice::from_ref(&capture.url),
            serde_json::Value::Object(options),
        )
        .await
        .expect("add");
    let status = wait_for_status(&engine, &gid).await;
    engine.shutdown().await;

    assert_eq!(
        std::fs::read(downloads.join("report.pdf")).unwrap(),
        b"the user's own report"
    );
    // aria2 now fails closed (error 13) instead of touching the file; the
    // controller avoids even that by downloading into a staging folder.
    assert_eq!(status.as_deref(), Some("error"));
    fixture.shutdown();
}
