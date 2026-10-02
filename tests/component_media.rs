mod common;

use std::path::Path;

use gray_discord::component_media::FileStore;
use gray_discord::durable::Store;
use tempfile::tempdir;

fn stores() -> (FileStore, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let store = Store::new(&dir.path().join("queue.sqlite3")).expect("store");
    let files = FileStore::new(
        store,
        dir.path().join("media"),
        dir.path().join("conversation"),
    )
    .expect("file store");
    (files, dir)
}

#[test]
fn import_is_private_deduplicated_and_exposed_only_through_metadata() {
    let (files, _dir) = stores();
    let first = files
        .import_bytes("user-1", "../hello world.txt", "text/plain", b"hello")
        .expect("import");
    let second = files
        .import_bytes("user-1", "different.txt", "text/plain", b"hello")
        .expect("duplicate import");
    assert_eq!(first.id, second.id);
    assert_eq!(first.name, "hello_world.txt");
    assert_eq!(files.metadata("user-1", &first.id).unwrap().size, 5);
    assert!(files.metadata("user-2", &first.id).is_err());
}

#[test]
fn generated_import_rejects_escape_and_accepts_conversation_file() {
    let (files, dir) = stores();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    assert!(files.import_generated("user-1", &outside).is_err());

    let workdir = dir.path().join("conversation");
    let generated = workdir.join("result.txt");
    std::fs::write(&generated, b"result").unwrap();
    let file = files
        .import_generated("user-1", Path::new(&generated))
        .unwrap();
    let safe = files
        .open_for_conversation("user-1", "chat:1", &file.id)
        .unwrap();
    assert!(safe.starts_with(&workdir));
    assert_eq!(std::fs::read(safe).unwrap(), b"result");
}

#[tokio::test]
async fn remote_private_hosts_are_rejected_and_multipart_part_is_available() {
    let (files, _dir) = stores();
    let blocked = files
        .import_url("user-1", "http://127.0.0.1/private.png")
        .await;
    assert!(blocked.is_err());
    let file = files
        .import_bytes("user-1", "ok.png", "image/png", b"png")
        .unwrap();
    assert!(files.multipart_part("user-1", &file.id).await.is_ok());
}

#[tokio::test]
async fn media_uploads_post_a_v2_gallery_with_attachment_metadata() {
    let stub = common::Stub::start().await;
    let rest = gray_discord::transport::Rest::new(&stub.base, "TESTTOKEN");
    let uploads = vec![
        gray_discord::media_tags::Upload {
            name: "chart.png".into(),
            media_type: "image/png".into(),
            bytes: b"png".to_vec(),
        },
        gray_discord::media_tags::Upload {
            name: "report.pdf".into(),
            media_type: "application/pdf".into(),
            bytes: b"%PDF".to_vec(),
        },
    ];
    let components = gray_discord::media_tags::components(&uploads, Some("**Results**"));
    rest.send_v2_uploads(42, &components, &uploads, None)
        .await
        .expect("upload sends");
    let sent = stub.sent.lock().unwrap().clone();
    let body = &sent.last().expect("request recorded").body;
    assert_eq!(body["flags"], 32768);
    assert_eq!(body["attachments"][0]["filename"], "chart.png");
    assert_eq!(body["attachments"][1]["filename"], "report.pdf");
    assert_eq!(body["components"][0]["content"], "**Results**");
    assert_eq!(
        body["components"][1]["items"][0]["media"]["url"],
        "attachment://chart.png"
    );
    assert_eq!(
        body["components"][2]["file"]["url"],
        "attachment://report.pdf"
    );
}

#[test]
fn discord_validation_errors_are_flattened_to_paths() {
    let body = serde_json::json!({
        "code": 50035,
        "message": "Invalid Form Body",
        "errors": {"components": {"0": {"components": {"1": {"spacing": {"_errors": [
            {"code": "NUMBER_TYPE_COERCE", "message": "Value \"small\" is not int."}
        ]}}}}}}
    });
    let detail = gray_discord::transport::discord_error_detail(400, &body);
    assert_eq!(
        detail,
        "Invalid Form Body: components[0].components[1].spacing: Value \"small\" is not int."
    );
}
