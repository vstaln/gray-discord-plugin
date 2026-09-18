use gray_discord::capabilities::prepare;
use std::path::Path;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("fixture write");
}

#[test]
fn explicit_skills_context_and_plugins_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("conversation");
    std::fs::create_dir(&home).unwrap();
    let skill = root.join("my-skill");
    std::fs::create_dir(&skill).unwrap();
    write(
        &skill.join("SKILL.md"),
        "---\nname: chosen\ndescription: chosen skill\n---\nhello\n",
    );
    let context = root.join("memory.md");
    write(&context, "Shared preference: concise");
    let config = serde_json::json!({
        "shared_skills": [skill.to_str().unwrap()],
        "shared_context": [context.to_str().unwrap()],
        "shared_plugins": [["/bin/cat"]],
    });
    let profile = prepare(&config, &home).expect("prepare");
    assert!(profile.contains("sidecar:"));
    assert!(glob_skill(&home), "linked skill must expose SKILL.md");
    assert!(std::fs::read_to_string(home.join("AGENTS.md"))
        .unwrap()
        .contains("Shared preference: concise"));
    // Empty config clears shared state, like the Python's second call.
    prepare(&serde_json::json!({}), &home).expect("clear");
    assert_eq!(std::fs::read_dir(home.join("skills")).unwrap().count(), 0);
    assert!(!home.join("AGENTS.md").exists());
}

fn glob_skill(home: &Path) -> bool {
    let skills = home.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills) else {
        return false;
    };
    for entry in entries.flatten() {
        if entry.path().join("SKILL.md").is_file() {
            return true;
        }
    }
    false
}

#[test]
fn rejects_missing_relative_or_oversize_context() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    for config in [
        serde_json::json!({"shared_skills": ["relative"]}),
        serde_json::json!({"shared_plugins": [[]]}),
        serde_json::json!({"shared_context": ["/missing/context"]}),
    ] {
        assert!(prepare(&config, &home).is_err(), "must reject {config}");
    }
    // Oversize file (context cap is 128 KiB total).
    let big = tmp.path().join("big.md");
    std::fs::write(&big, vec![b'x'; 128 * 1024 + 1]).unwrap();
    let config = serde_json::json!({"shared_context": [big.to_str().unwrap()]});
    let err = prepare(&config, &home).unwrap_err();
    assert_eq!(err, "Shared context exceeds 128 KiB; select smaller files");
}
