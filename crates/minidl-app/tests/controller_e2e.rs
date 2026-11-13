//! Full controller-to-aria2 completion flow using a deterministic loopback
//! fixture. Ignored by default because `aria2c` must be on PATH.

use std::path::PathBuf;
use std::time::Duration;

use minidl_app::controller::DownloadView;
use minidl_app::Controller;
use minidl_core::model::DownloadStatus;
use minidl_ipc::ControlCommand;

#[path = "../../../test-support/loopback_http.rs"]
mod loopback_http;

use loopback_http::LoopbackHttp;

#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn ingest_url_reaches_controller_completion() {
    let fixture = LoopbackHttp::start(768 * 1_024).await;
    let root = tempfile::tempdir().expect("temporary controller directory");
    let downloads = root.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();

    let controller = Controller::open(root.path().join("data"), downloads)
        .await
        .expect("open controller");
    // Loopback targets are used only by this deterministic test fixture.
    controller
        .db()
        .set_setting("block_private_ips", "false")
        .unwrap();

    let inserted: DownloadView = serde_json::from_value(
        controller
            .execute(ControlCommand::IngestUrl {
                url: fixture.url.clone(),
                checksum: None,
            })
            .await
            .expect("ingest loopback URL"),
    )
    .expect("download response");

    let completed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(row) = controller
                .snapshot()
                .await
                .expect("controller snapshot")
                .downloads
                .into_iter()
                .find(|row| row.id == inserted.id)
            {
                if row.status == DownloadStatus::Complete {
                    break Some(row);
                }
                if row.status == DownloadStatus::Error {
                    panic!("controller download failed: {:?}", row.error_message);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(None);

    let _ = controller.execute(ControlCommand::Quit).await;
    let completed = completed.expect("controller download did not complete in time");
    assert_eq!(completed.total_bytes as usize, fixture.payload.len());
    assert_eq!(completed.completed_bytes, completed.total_bytes);
    let path = PathBuf::from(completed.dir).join(completed.filename.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), fixture.payload.as_slice());
    fixture.shutdown();
}

async fn open(root: &std::path::Path) -> (std::sync::Arc<Controller>, PathBuf) {
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let controller = Controller::open(root.join("data"), downloads.clone())
        .await
        .expect("open controller");
    // Loopback targets are used only by these deterministic test fixtures.
    controller
        .db()
        .set_setting("block_private_ips", "false")
        .unwrap();
    (controller, downloads)
}

fn capture(url: &str, filename: Option<&str>) -> minidl_ipc::CaptureJob {
    let mut job = minidl_app::ingest::job_from_url(url.into());
    job.filename = filename.map(Into::into);
    job
}

async fn wait_terminal(controller: &Controller, id: i64, seconds: u64) -> Option<DownloadView> {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            let row = controller
                .snapshot()
                .await
                .expect("controller snapshot")
                .downloads
                .into_iter()
                .find(|row| row.id == id);
            if let Some(row) = row.filter(|row| row.status.is_terminal()) {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .ok()
}

/// Audit HIGH #1 through the whole controller: the capture's cookie and
/// Authorization reach the origin they were captured for, never the host it
/// redirects to.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn browser_credentials_do_not_follow_a_cross_origin_redirect() {
    let target = LoopbackHttp::start(200 * 1_024).await;
    let origin = LoopbackHttp::redirect_to(&target.url).await;
    let root = tempfile::tempdir().unwrap();
    let (controller, _) = open(root.path()).await;
    let mut job = capture(&origin.url, Some("private.bin"));
    job.cookie = Some("sid=SECRET".into());
    job.extra_headers = vec![("Authorization".into(), "Bearer TOKEN".into())];

    let id = controller.ingest_capture(job, None).await.expect("ingest");
    let row = wait_terminal(&controller, id, 15).await;
    let _ = controller.execute(ControlCommand::Quit).await;

    assert!(origin.saw_header("cookie"));
    assert!(
        !target.saw_header("cookie"),
        "cookie leaked to the redirect target"
    );
    assert!(!target.saw_header("authorization"), "authorization leaked");
    let row = row.expect("download finished");
    assert_eq!(
        row.status,
        DownloadStatus::Complete,
        "{:?}",
        row.error_message
    );
    let path = PathBuf::from(&row.dir).join(row.filename.unwrap());
    assert_eq!(std::fs::read(path).unwrap(), target.payload.as_slice());
}

/// Audit HIGH #2 through the whole controller: a capture named like a file
/// the user owns lands next to it, and the user's file is untouched.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn a_capture_never_overwrites_a_same_named_file() {
    let fixture = LoopbackHttp::start(300 * 1_024).await;
    let root = tempfile::tempdir().unwrap();
    let (controller, downloads) = open(root.path()).await;
    // A category would move the result elsewhere; keep it in Downloads.
    controller
        .db()
        .set_setting("auto_organize", "false")
        .unwrap();
    std::fs::write(downloads.join("report.bin"), b"the user's own file").unwrap();

    // Duplicate prevention (the default) fails closed on the taken name.
    let refused = controller
        .ingest_capture(capture(&fixture.url, Some("report.bin")), None)
        .await
        .expect("ingest");
    let refused = wait_terminal(&controller, refused, 15)
        .await
        .expect("download finished");
    assert_eq!(refused.status, DownloadStatus::Error);
    assert!(refused
        .error_message
        .as_deref()
        .is_some_and(|message| message.contains("duplicate prevention")));
    assert!(!downloads.join("report (1).bin").exists());

    // With it off, the download lands next to the user's file.
    controller
        .db()
        .set_setting("prevent_duplicate_downloads", "false")
        .unwrap();
    let id = controller
        .ingest_capture(capture(&fixture.url, Some("report.bin")), None)
        .await
        .expect("ingest");
    let row = wait_terminal(&controller, id, 15).await;
    let _ = controller.execute(ControlCommand::Quit).await;

    assert_eq!(
        std::fs::read(downloads.join("report.bin")).unwrap(),
        b"the user's own file"
    );
    let row = row.expect("download finished");
    assert_eq!(
        row.status,
        DownloadStatus::Complete,
        "{:?}",
        row.error_message
    );
    assert_eq!(row.filename.as_deref(), Some("report (1).bin"));
    assert_eq!(
        std::fs::read(downloads.join("report (1).bin")).unwrap(),
        fixture.payload.as_slice()
    );
    assert!(
        !minidl_core::files::stage_dir(&downloads, id).exists(),
        "staging folder left behind"
    );
    // The refused download's bytes wait in its own staging folder, as its
    // error message says.
    assert!(minidl_core::files::stage_dir(&downloads, refused.id)
        .join("report.bin")
        .exists());
}

/// Audit MEDIUM: a job that finishes before the code that added it had bound
/// its GID used to leave the row "active" forever.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn instant_completions_never_leave_a_row_active() {
    let fixture = LoopbackHttp::start(1).await;
    let root = tempfile::tempdir().unwrap();
    let (controller, _) = open(root.path()).await;
    controller
        .db()
        .set_setting("prevent_duplicate_downloads", "false")
        .unwrap();
    let mut ids = Vec::new();
    for index in 0..25 {
        let name = format!("tiny-{index}.bin");
        ids.push(
            controller
                .ingest_capture(capture(&fixture.url, Some(&name)), None)
                .await
                .expect("ingest"),
        );
    }
    let mut stuck = Vec::new();
    for id in ids {
        match wait_terminal(&controller, id, 10).await {
            Some(row) if row.status == DownloadStatus::Complete => {}
            other => stuck.push((id, other.map(|row| row.status))),
        }
    }
    let _ = controller.execute(ControlCommand::Quit).await;
    assert!(stuck.is_empty(), "rows not complete: {stuck:?}");
}

/// Audit MEDIUM: aria2 is killed mid-transfer. The respawned engine must
/// still finish the job (session saved at add time, rows reconciled after
/// the respawn) instead of leaving its row "active" at 0 B/s.
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn a_download_survives_an_aria2_crash() {
    let fixture = LoopbackHttp::start_slow(1_024 * 1_024, Duration::from_millis(40)).await;
    let root = tempfile::tempdir().unwrap();
    let (controller, downloads) = open(root.path()).await;
    controller
        .db()
        .set_setting("auto_organize", "false")
        .unwrap();
    let id = controller
        .ingest_capture(capture(&fixture.url, Some("big.bin")), None)
        .await
        .expect("ingest");
    tokio::time::sleep(Duration::from_millis(600)).await;
    // The engine's aria2c is the only one started with this data dir.
    let data = root.path().join("data");
    let pid = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            std::fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| {
                String::from_utf8_lossy(&cmdline).contains(&*data.to_string_lossy())
            })
        })
        .expect("running aria2c")
        .file_name();
    std::process::Command::new("kill")
        .arg("-9")
        .arg(&pid)
        .status()
        .unwrap();

    let row = wait_terminal(&controller, id, 40).await;
    let why = controller
        .execute(ControlCommand::Why { id })
        .await
        .unwrap();
    let _ = controller.execute(ControlCommand::Quit).await;
    let row = row.expect("row stuck after the aria2 crash");
    assert_eq!(row.status, DownloadStatus::Complete, "{why}");
    assert_eq!(
        std::fs::read(downloads.join("big.bin")).unwrap(),
        fixture.payload.as_slice()
    );
    assert!(why["summary"].as_str().unwrap().contains("complete"));
}

/// Audit review: a staged download that aria2 finished while the controller
/// was not watching is moved into place by reconcile, not left hidden in
/// `.minidl-staging/<id>` with the row marked complete.
#[tokio::test]
#[ignore = "requires aria2c on PATH"]
async fn a_download_finished_unwatched_is_moved_into_place_by_reconcile() {
    use minidl_core::aria2::{build_add_options, new_gid, Engine, EngineDefaults, LaunchOptions};
    use minidl_core::db::Db;
    use minidl_core::model::NewDownload;

    let fixture = LoopbackHttp::start(64 * 1_024).await;
    let root = tempfile::tempdir().unwrap();
    let downloads = root.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let db = Db::open(&root.path().join("data")).unwrap();
    let engine = Engine::launch(LaunchOptions {
        download_dir: downloads.clone(),
        data_dir: root.path().join("data"),
        ..Default::default()
    })
    .await
    .expect("engine launch");

    let id = db
        .insert_download(&NewDownload {
            url: fixture.url.clone(),
            filename: Some("late.bin".into()),
            dir: downloads.to_string_lossy().into_owned(),
            kind: "http".into(),
            ..Default::default()
        })
        .unwrap();
    let stage = minidl_core::files::create_stage_dir(&downloads, id).unwrap();
    db.set_stage_dir(id, Some(&stage.to_string_lossy()))
        .unwrap();
    let gid = new_gid().unwrap();
    db.bind_aria2_job(id, &gid).unwrap();
    db.transition(id, DownloadStatus::Active, "test").unwrap();
    let mut options = build_add_options(
        &capture(&fixture.url, Some("late.bin")),
        &stage.to_string_lossy(),
        &EngineDefaults::default(),
    );
    options.insert("gid".into(), gid.clone().into());
    engine
        .rpc
        .add_uri(
            std::slice::from_ref(&fixture.url),
            serde_json::Value::Object(options),
        )
        .await
        .expect("add");
    // No controller is listening: aria2 finishes on its own.
    tokio::time::timeout(Duration::from_secs(15), async {
        while engine
            .rpc
            .tell_status(&gid, &["status"])
            .await
            .ok()
            .and_then(|status| status["status"].as_str().map(str::to_owned))
            .as_deref()
            != Some("complete")
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("aria2 finished");

    minidl_app::reconcile::reconcile(&engine, &db).await;
    engine.shutdown().await;

    let row = db.get(id).unwrap().unwrap();
    assert_eq!(row.status, DownloadStatus::Complete);
    assert_eq!(
        std::fs::read(downloads.join("late.bin")).unwrap(),
        fixture.payload.as_slice()
    );
    assert!(row.stage_dir.is_none());
    assert!(!downloads.join(minidl_core::files::STAGING_DIR).exists());
}
