use std::process::Command;

#[test]
fn help_lists_subcommands() {
    let exe = env!("CARGO_BIN_EXE_gray-discord");
    let out = Command::new(exe).arg("--help").output().expect("spawn");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    for cmd in [
        "setup",
        "run",
        "sidecar",
        "register",
        "doctor",
        "schedule",
        "allowlist",
    ] {
        assert!(text.contains(cmd), "help missing {cmd}");
    }
}
