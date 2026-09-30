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
