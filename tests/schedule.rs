use gray_discord::config::save_config;
use gray_discord::gateway::open_store;
use serde_json::json;
use std::os::unix::io::AsRawFd;
use std::process::Command;

#[test]
fn online_crud_via_store_and_cli() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    save_config(
        &path,
        &json!({
            "token": "TESTTOKEN",
            "owner_id": "123",
            "channel_id": "42",
            "gray_bin": "/bin/true",
            "gray_home": tmp.path(),
            "workdir": tmp.path(),
        }),
    )
    .unwrap();

    // Hold the gateway lock open while running CLI commands (online CRUD)
    let lock_path = path.parent().unwrap().join("gateway.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)
        .unwrap();
    let locked = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    assert!(locked, "must hold gateway lock");
    let _lock = lock_file;

    let store = open_store(&path).unwrap();
    store
        .schedule_add("legacy-job", 120, "legacy", 1000.0)
        .unwrap();
    let migrated = open_store(&path).unwrap();
    let jobs = migrated.schedules().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "legacy-job");

    let invoke = |args: &[&str]| {
        Command::new(exe)
            .arg("--config")
            .arg(&path)
            .arg("schedule")
            .args(args)
            .output()
            .expect("spawn")
    };

    // --every 0 must fail
    let out = invoke(&["add", "--every", "0", "hello"]);
    assert_eq!(out.status.code(), Some(1));

    // --every 60 succeeds and prints job ID
    let out = invoke(&["add", "--every", "60", "hello"]);
    assert_eq!(out.status.code(), Some(0));
    let job_id = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert!(!job_id.is_empty());

    // list contains job_id
    let out = invoke(&["list"]);
    let list = String::from_utf8(out.stdout).unwrap();
    assert!(list.contains(&job_id));

    let all_jobs = store.schedules().unwrap();
    let created = all_jobs.iter().find(|j| j.id == job_id).expect("found job");
    assert_eq!(created.prompt, "hello");

    // remove job_id
    let out = invoke(&["remove", &job_id]);
    assert_eq!(out.status.code(), Some(0));

    // list now only has legacy-job
    let out = invoke(&["list"]);
    let list = String::from_utf8(out.stdout).unwrap();
    assert_eq!(list, "legacy-job 120 scheduled\n");

    // remove again fails
    let out = invoke(&["remove", &job_id]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn allowlist_cli_add_list_remove() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    save_config(
        &path,
        &json!({
            "token": "TESTTOKEN",
            "owner_id": "123",
            "channel_id": "42",
            "gray_bin": "/bin/true",
            "gray_home": tmp.path(),
            "workdir": tmp.path(),
        }),
    )
    .unwrap();

    let invoke = |args: &[&str]| {
        Command::new(exe)
            .arg("--config")
            .arg(&path)
            .arg("allowlist")
            .args(args)
            .output()
            .expect("spawn")
    };

    // Initially empty
    let out = invoke(&["list"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "");

    // Add valid snowflake
    let out = invoke(&["add", "12345678901234567"]);
    assert_eq!(out.status.code(), Some(0));

    // Add invalid snowflake fails
    let out = invoke(&["add", "invalid_id"]);
    assert_eq!(out.status.code(), Some(1));

    // List shows the added snowflake
    let out = invoke(&["list"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "12345678901234567\n"
    );

    // Remove valid snowflake
    let out = invoke(&["remove", "12345678901234567"]);
    assert_eq!(out.status.code(), Some(0));

    // List is empty again
    let out = invoke(&["list"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "");
}

#[test]
fn error_envelope_secrecy_and_no_traceback() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    // Write invalid JSON config
    std::fs::write(&path, "{\"token\": \"SECRET123\", bad}").unwrap();

    // A command that loads the config fails closed with a sanitized error.
    let out = Command::new(exe)
        .arg("--config")
        .arg(&path)
        .arg("doctor")
        .output()
        .expect("spawn");

    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("Traceback"));
    assert!(!err.contains("SECRET123"));

    // `status` needs no config: with no service installed under this box's
    // supervisor it reports "not installed" and exits 0 (it is a report,
    // not a failure).
    let path2 = tmp.path().join("other-config.json");
    let out = Command::new(exe)
        .arg("--config")
        .arg(&path2)
        .arg("status")
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let out_text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_text.contains("not installed")
            || out_text.contains("runs")
            || out_text.contains("not running")
    );
}
