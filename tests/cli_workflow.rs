use std::path::Path;

use serde_json::Value;

fn command(home: &Path) -> assert_cmd::Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("ctx");
    command
        .env("CONTEXTWAKE_HOME", home)
        .env("CONTEXTWAKE_CODEX_BIN", "contextwake-test-missing-codex")
        .env("CONTEXTWAKE_CLAUDE_BIN", "contextwake-test-missing-claude");
    command
}

fn json_output(home: &Path, args: &[&str]) -> Value {
    let output = command(home)
        .arg("--json")
        .args(args)
        .output()
        .expect("run ctx");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

#[test]
fn canonical_cli_reports_contextwake_version_and_help() {
    let version = assert_cmd::cargo::cargo_bin_cmd!("ctx")
        .arg("--version")
        .output()
        .expect("run ctx --version");
    assert!(version.status.success());
    let version_text = String::from_utf8_lossy(&version.stdout);
    assert!(version_text.starts_with("ctx "));
    assert!(version_text.contains(env!("CARGO_PKG_VERSION")));

    let help = assert_cmd::cargo::cargo_bin_cmd!("ctx")
        .arg("--help")
        .output()
        .expect("run ctx --help");
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("Usage: ctx"));
    assert!(help_text.contains("ContextWake"));
}

#[test]
fn legacy_cli_alias_reports_the_same_version() {
    let output = assert_cmd::cargo::cargo_bin_cmd!("ctxwake")
        .arg("--version")
        .output()
        .expect("run ctxwake --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("ctxwake "));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));

    let help = assert_cmd::cargo::cargo_bin_cmd!("ctxwake")
        .arg("--help")
        .output()
        .expect("run ctxwake --help");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: ctxwake"));
}

#[test]
fn legacy_home_override_remains_compatible_after_rename() {
    let root = tempfile::tempdir().expect("root");
    let legacy_home = root.path().join("legacy-home");
    let output = assert_cmd::cargo::cargo_bin_cmd!("ctxwake")
        .env_remove("CONTEXTWAKE_HOME")
        .env("AGENTDECK_HOME", &legacy_home)
        .args(["config", "path"])
        .output()
        .expect("run ctxwake compatibility alias with legacy home");
    assert!(output.status.success());
    let reported = String::from_utf8(output.stdout).expect("UTF-8 path");
    assert!(reported.contains(&legacy_home.to_string_lossy().to_string()));
    assert!(legacy_home.join("data/state.sqlite3").is_file());
}

#[test]
fn status_without_a_profile_has_no_implicit_agent() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");

    let status = json_output(&home, &["status", &workspace.to_string_lossy()]);
    assert!(status["active_profile"].is_null());
    assert_eq!(status["agent"]["agent_id"], "unconfigured");
    assert_eq!(status["agent"]["installed"], false);
    assert_eq!(status["agent"]["auth_state"], "unknown");
}

#[test]
fn profile_workspace_checkpoint_and_handoff_survive_restart() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let workspace = root.path().join("ordinary workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(workspace.join(".contextwake")).expect("project config directory");
    std::fs::write(
        workspace.join(".contextwake/project.toml"),
        r#"schema_version = 1
instructions_file = "AGENTS.md"
[[validation]]
executable = "git"
args = ["--version"]
"#,
    )
    .expect("project config");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "Keep continuity explicit. \u{1b}[31m sk-abcdefghijklmnopqrstuvwxyz",
    )
    .expect("project instructions");

    let profile = json_output(&home, &["profile", "add", "Personal"]);
    assert_eq!(profile["name"], "personal");
    assert_eq!(profile["display_name"], "Personal");
    assert_eq!(profile["auth_state"], "signed_out");
    let config = json_output(
        &home,
        &[
            "config",
            "set",
            "--ascii",
            "true",
            "--validation-timeout-ms",
            "30000",
        ],
    );
    assert_eq!(config["ascii"], true);
    assert_eq!(config["validation_timeout_ms"], 30_000);
    assert_eq!(config["telemetry"], false);

    let workspace_text = workspace.to_string_lossy();
    let registered = json_output(
        &home,
        &["workspace", "add", workspace_text.as_ref(), "--trust"],
    );
    assert_eq!(registered["trust_state"], "trusted");
    let validation = json_output(
        &home,
        &[
            "workspace",
            "validate",
            "--workspace",
            workspace_text.as_ref(),
        ],
    );
    assert_eq!(validation[0]["status"], "passed");

    let checkpoint = json_output(
        &home,
        &[
            "checkpoint",
            "create",
            "--objective",
            "Continue implementation with sk-abcdefghijklmnopqrstuvwxyz",
            "--workspace",
            workspace_text.as_ref(),
            "--pending",
            "Run tests",
            "--validate",
        ],
    );
    assert_eq!(checkpoint["redaction_status"], "redacted");
    assert!(
        !checkpoint["objective"]
            .as_str()
            .expect("objective")
            .contains("sk-")
    );
    assert_eq!(checkpoint["validation_results"][0]["status"], "passed");
    assert!(
        checkpoint["project_instructions"]
            .as_str()
            .expect("instructions")
            .contains("[REDACTED]")
    );
    assert!(
        !checkpoint["project_instructions"]
            .as_str()
            .expect("instructions")
            .contains('\u{1b}')
    );
    let status = json_output(&home, &["status", workspace_text.as_ref()]);
    assert_eq!(status["continuity_guardian"]["level"], "current");
    assert_eq!(
        status["continuity_guardian"]["latest_checkpoint_id"],
        checkpoint["id"]
    );
    let checkpoint_id = checkpoint["id"].as_str().expect("checkpoint id");

    let handoff = json_output(&home, &["handoff", "create", checkpoint_id]);
    let handoff_id = handoff["id"].as_str().expect("handoff id");
    let manifest = json_output(&home, &["handoff", "show", handoff_id]);
    assert_eq!(manifest["schema_version"], "1.2.0");
    assert_eq!(manifest["continuity_mode"], "portable_handoff");
    assert_eq!(manifest["source"]["agent_id"], "codex");
    assert!(manifest["source"]["model_provider_id"].is_null());
    assert_eq!(manifest["workspace"]["display_name"], "ordinary workspace");
    assert!(
        manifest["security_flags"]
            .as_array()
            .expect("security flags")
            .iter()
            .any(|flag| flag == "secrets_redacted")
    );

    // A fresh process reads the same SQLite/config state.
    let profiles = json_output(&home, &["profile", "list"]);
    assert_eq!(profiles.as_array().expect("profiles").len(), 1);
    let checkpoints = json_output(&home, &["checkpoint", "list"]);
    assert_eq!(checkpoints.as_array().expect("checkpoints").len(), 1);
    let handoffs = json_output(&home, &["handoff", "list"]);
    assert_eq!(handoffs.as_array().expect("handoffs").len(), 1);

    let exported = root.path().join("exported-handoff");
    let export_output = command(&home)
        .args([
            "handoff",
            "export",
            handoff_id,
            exported.to_string_lossy().as_ref(),
        ])
        .output()
        .expect("export");
    assert!(export_output.status.success());
    let exported_context =
        std::fs::read_to_string(exported.join("context.md")).expect("exported context");
    assert!(exported_context.contains("## Project Instructions"));
    assert!(!exported_context.contains("sk-"));

    let imported_home = root.path().join("imported-home");
    let imported = json_output(
        &imported_home,
        &["handoff", "import", exported.to_string_lossy().as_ref()],
    );
    assert_eq!(imported["id"], handoff_id);
    assert_eq!(
        imported["checkpoint_id"],
        "00000000-0000-0000-0000-000000000000"
    );
    let imported_manifest = json_output(&imported_home, &["handoff", "show", handoff_id]);
    let imported_objective = imported_manifest["objective"]
        .as_str()
        .expect("imported objective");
    assert!(imported_objective.contains("[REDACTED]"));
    assert!(!imported_objective.contains("sk-"));

    std::fs::write(exported.join("context.md"), "tampered").expect("tamper");
    let rejected_home = root.path().join("rejected-home");
    let rejected = command(&rejected_home)
        .arg("--json")
        .args(["handoff", "import", exported.to_string_lossy().as_ref()])
        .output()
        .expect("rejected import");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("integrity check failed"));
}

#[test]
fn errors_are_structured_and_do_not_echo_terminal_control_sequences() {
    let root = tempfile::tempdir().expect("root");
    let output = command(root.path())
        .arg("--json")
        .args(["profile", "show", "\u{1b}[31mmissing"])
        .output()
        .expect("run ctx");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains('\u{1b}'));
    let error: Value = serde_json::from_str(&stderr).expect("structured JSON error");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("missing")
    );
    assert_eq!(error["code"], "CWK-PROFILE-NOT-FOUND");
}

#[test]
fn agent_and_model_are_distinct_and_persisted() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let profile = json_output(
        &home,
        &[
            "profile",
            "add",
            "Work",
            "--agent",
            "claude",
            "--model-provider",
            "anthropic",
            "--model",
            "sonnet",
        ],
    );
    assert_eq!(profile["agent_id"], "claude");
    assert_eq!(profile["model_provider_id"], "anthropic");
    assert_eq!(profile["model_preference"], "sonnet");

    let duplicate = command(&home)
        .args(["profile", "add", "Work", "--agent", "claude"])
        .output()
        .expect("duplicate profile attempt");
    assert!(!duplicate.status.success());
    assert_eq!(
        std::fs::read_dir(home.join("data/agent-homes/claude"))
            .expect("agent homes")
            .count(),
        1,
        "failed profile creation must clean up its uncommitted agent home"
    );

    let updated = json_output(
        &home,
        &[
            "model",
            "select",
            "opus",
            "--provider",
            "anthropic",
            "--profile",
            "work",
        ],
    );
    assert_eq!(updated["agent_id"], "claude");
    assert_eq!(updated["model_provider_id"], "anthropic");
    assert_eq!(updated["model_preference"], "opus");

    let model_view = json_output(&home, &["model", "list"]);
    assert_eq!(model_view["agent_id"], "claude");
    assert_eq!(model_view["selected_provider"], "anthropic");
    assert_eq!(model_view["selected_model"], "opus");
    assert_eq!(model_view["available_model_listing"]["support"], "partial");

    let agents = json_output(&home, &["agent", "list"]);
    let agents = agents.as_array().expect("agent list");
    assert_eq!(agents.len(), 9);
    for expected in [
        "codex",
        "claude",
        "github-copilot",
        "cursor",
        "opencode",
        "gemini",
        "kiro",
        "kimi",
        "grok",
    ] {
        assert!(agents.iter().any(|agent| agent["id"] == expected));
    }
    assert!(
        agents.iter().all(|agent| {
            agent["adapter"] == "implemented" && agent["capabilities"].is_object()
        })
    );
}

#[test]
fn unavailable_target_agent_does_not_change_active_profile() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    json_output(&home, &["profile", "add", "Personal", "--agent", "codex"]);
    json_output(&home, &["profile", "add", "Work", "--agent", "claude"]);

    let failed = command(&home)
        .args(["profile", "use", "work"])
        .output()
        .expect("switch attempt");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("active profile was not changed"));

    let status = json_output(&home, &["status", workspace.to_string_lossy().as_ref()]);
    assert_eq!(status["active_profile"]["name"], "personal");
    assert_eq!(status["active_profile"]["agent_id"], "codex");
}
