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
