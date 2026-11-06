//! State-root handling of the `ctx` binary.

#[test]
fn relative_state_root_is_refused_without_creating_state() {
    let root = tempfile::tempdir().expect("root");
    let output = assert_cmd::cargo::cargo_bin_cmd!("ctx")
        .current_dir(root.path())
        .env("CONTEXTWAKE_HOME", "relative-state")
        .env_remove("AGENTDECK_HOME")
        .env("CONTEXTWAKE_CODEX_BIN", "contextwake-test-missing-codex")
        .env("CONTEXTWAKE_CLAUDE_BIN", "contextwake-test-missing-claude")
        .args(["--json", "config", "path"])
        .output()
        .expect("run ctx");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CONTEXTWAKE_HOME must be an absolute path"),
        "{stderr}"
    );
    assert!(
        !root.path().join("relative-state").exists(),
        "no state may be created relative to the working directory"
    );
}

#[test]
fn state_root_override_is_used_in_conventional_form() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state home");
    let mut spelled = home.to_string_lossy().into_owned();
    if cfg!(windows) {
        spelled = format!(r"\\?\{spelled}");
    }
    let output = assert_cmd::cargo::cargo_bin_cmd!("ctx")
        .env("CONTEXTWAKE_HOME", &spelled)
        .env_remove("AGENTDECK_HOME")
        .env("CONTEXTWAKE_CODEX_BIN", "contextwake-test-missing-codex")
        .env("CONTEXTWAKE_CLAUDE_BIN", "contextwake-test-missing-claude")
        .args(["config", "path"])
        .output()
        .expect("run ctx");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = String::from_utf8(output.stdout).expect("UTF-8 path");
    assert_eq!(
        reported.trim(),
        home.join("config")
            .join("config.toml")
            .to_string_lossy()
            .as_ref()
    );
    assert!(home.join("data").join("state.sqlite3").is_file());
}
