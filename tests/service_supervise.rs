//! Service lifecycle across every supervisor this box might have. The ops
//! take an injected `Supervisor`, so the runit and no-init paths are proven
//! against fixture directories without touching the real service tree.

use gray_discord::service::{self, Supervisor, SVC};
use std::path::{Path, PathBuf};

fn exe_fixture(dir: &Path) -> PathBuf {
    // A stable, executable stand-in: the ops write it into the run script
    // exactly as recorded (they never invoke it in these tests).
    let path = dir.join("fake-gray-discord");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn runit_script_quotes_every_word() {
    let script = service::runit_script(&[
        "/opt/gray discord".into(),
        "run".into(),
        "--config".into(),
        "/a'b".into(),
    ]);
    assert_eq!(
        script,
        "#!/bin/sh\nexec '/opt/gray discord' 'run' '--config' '/a'\\''b'\n"
    );
}

#[test]
fn svdir_resolution_order() {
    // $SVDIR wins, then ~/.config/service, then the system tree (same order
    // as gray's gateway detection).
    assert_eq!(
        service::svdir_from(Some("/tmp/sv".into()), Some("/home/u".into())),
        PathBuf::from("/tmp/sv")
    );
    assert_eq!(
        service::svdir_from(None, Some("/home/u".into())),
        PathBuf::from("/home/u/.config/service")
    );
    assert_eq!(
        service::svdir_from(None, None),
        PathBuf::from("/var/service")
    );
}

#[test]
fn quote_escapes_like_the_python() {
    assert_eq!(
        service::quote("a%b$c\\d\"e").unwrap(),
        "\"a%%b$$c\\\\d\\\"e\""
    );
    assert!(service::quote("a\nb").is_err());
}

#[test]
fn unit_mentions_the_config_and_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = exe_fixture(tmp.path());
    std::env::set_var("GRAY_DISCORD_TEST_EXE", &exe);
    // unit() uses current_exe, so prove the shape with daemon argv pieces:
    // the unit must carry `run --config <path>` under ExecStart.
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    let body = service::unit(&cfg).unwrap();
    assert!(body.contains("ExecStart="));
    assert!(body.contains("\"run\" \"--config\" "));
    assert!(body.contains(&format!("\"{}\"", cfg.display())));
    assert!(body.contains("WantedBy=default.target"));
}

#[test]
fn runit_install_writes_the_script_before_calling_sv() {
    let tmp = tempfile::tempdir().unwrap();
    let svc_root = tmp.path().join("service");
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    let sup = Supervisor::Runit {
        dir: svc_root.clone(),
    };
    // `sv up` runs against a fixture tree (harmless: it fails on a service
    // it cannot supervise), but the run script must already be on disk —
    // that is the install contract: write, then bring up.
    let _ = service::install_with(sup.clone(), &cfg);
    let run = svc_root.join(SVC).join("run");
    let body = std::fs::read_to_string(&run).unwrap();
    assert!(body.starts_with("#!/bin/sh\nexec "));
    assert!(body.contains("run"));
    assert!(body.contains("config.json"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&run).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn runit_status_without_a_service_says_not_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    let sup = Supervisor::Runit {
        dir: tmp.path().join("service"),
    };
    let line = service::status_with(sup, &cfg).unwrap();
    assert!(line.contains("not installed"));
}

#[test]
fn none_supervisor_status_without_running_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    let line = service::status_with(Supervisor::None, &cfg).unwrap();
    assert!(line.contains("not running"));
    assert!(line.contains("gray-discord.pid"));
}

#[test]
fn none_supervisor_stop_reports_missing_pidfile() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    let err = service::stop_with(Supervisor::None, &cfg).unwrap_err();
    assert!(err.contains("no pid file"));
}

#[test]
fn none_supervisor_stop_signals_recorded_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.json");
    std::fs::write(&cfg, "{}").unwrap();
    // Claim an already-dead pid: `kill 0` fails, so stop must fail loudly
    // rather than report a daemon it did not stop.
    std::fs::write(cfg.parent().unwrap().join("gray-discord.pid"), "4194303\n").unwrap();
    // 4194303 is above the default pid_max; if it somehow exists, skip.
    let proc_exists = Path::new("/proc/4194303").exists();
    if !proc_exists {
        let err = service::stop_with(Supervisor::None, &cfg).unwrap_err();
        assert!(err.contains("could not signal pid"));
    }
}

#[test]
fn detect_matches_this_box() {
    // Deterministic part of the contract: the enum must reflect reality.
    match service::detect() {
        Supervisor::Runit { dir } => {
            assert!(dir.ends_with("service") || dir.starts_with("/var/service"));
            assert!(std::path::Path::new("/usr/bin/sv").is_file());
        }
        Supervisor::SystemdUser { unit_dir } => {
            assert!(unit_dir.ends_with("systemd/user"));
        }
        Supervisor::None => {}
    }
}
