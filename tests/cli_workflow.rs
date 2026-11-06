use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use contextwake::paths::canonicalize_simplified;
use serde_json::Value;

/// Every agent override points at a missing executable, so no test ever runs
/// an installed coding agent or touches its real home.
const AGENT_OVERRIDES: [&str; 9] = [
    "CONTEXTWAKE_CODEX_BIN",
    "CONTEXTWAKE_CLAUDE_BIN",
    "CONTEXTWAKE_COPILOT_BIN",
    "CONTEXTWAKE_CURSOR_BIN",
    "CONTEXTWAKE_GEMINI_BIN",
    "CONTEXTWAKE_GROK_BIN",
    "CONTEXTWAKE_KIMI_BIN",
    "CONTEXTWAKE_KIRO_BIN",
    "CONTEXTWAKE_OPENCODE_BIN",
];

fn command(home: &Path) -> assert_cmd::Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("ctx");
    command.env("CONTEXTWAKE_HOME", home);
    for variable in AGENT_OVERRIDES {
        command.env(variable, format!("contextwake-test-missing-{variable}"));
    }
    command
}

fn command_in(home: &Path, directory: &Path) -> assert_cmd::Command {
    let mut command = command(home);
    command.current_dir(directory);
    command
}

/// Runs `ctx` from `directory` with `user_home` as the user's home directory.
fn command_at_home(home: &Path, user_home: &Path, directory: &Path) -> assert_cmd::Command {
    let mut command = command_in(home, directory);
    command.env("USERPROFILE", user_home).env("HOME", user_home);
    command
}

fn json_output(home: &Path, args: &[&str]) -> Value {
    successful_json(
        command(home)
            .arg("--json")
            .args(args)
            .output()
            .expect("run ctx"),
    )
}

fn json_in(home: &Path, directory: &Path, args: &[&str]) -> Value {
    successful_json(
        command_in(home, directory)
            .arg("--json")
            .args(args)
            .output()
            .expect("run ctx"),
    )
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

/// The structured `--json` error of a command that must fail.
fn json_error(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stderr).expect("structured JSON error")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn canonical(path: &Path) -> PathBuf {
    canonicalize_simplified(path).expect("canonical path")
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn git_repository(path: &Path) {
    std::fs::create_dir_all(path).expect("repository directory");
    let status = std::process::Command::new("git")
        .current_dir(path)
        .args(["init", "--quiet"])
        .stdin(std::process::Stdio::null())
        .status()
        .expect("Git must be installed for workspace integration tests");
    assert!(status.success(), "git init failed");
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("doctor has no {name} check: {report}"))
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

#[test]
fn commands_without_workspace_target_the_current_directory_not_the_active_workspace() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let repo_a = root.path().join("repo-a");
    let repo_b = root.path().join("repo-b");
    git_repository(&repo_a);
    git_repository(&repo_b);
    std::fs::create_dir_all(repo_b.join("src")).expect("subdirectory");

    let first = json_in(
        &home,
        &repo_a,
        &["checkpoint", "create", "--objective", "Work in A"],
    );
    // Repository A is now the active workspace; work recorded in B must stay in B.
    let second = json_in(
        &home,
        &repo_b.join("src"),
        &[
            "checkpoint",
            "create",
            "--objective",
            "Add CSV export",
            "--task",
            "Implement field quoting",
            "--decision",
            "Use a streaming writer",
        ],
    );
    assert_ne!(
        second["workspace_id"], first["workspace_id"],
        "a checkpoint created in repository B was stored for the active workspace A"
    );

    let status_b = json_in(&home, &repo_b, &["status"]);
    assert_eq!(status_b["workspace"]["id"], second["workspace_id"]);
    assert_eq!(status_b["workspace"]["display_name"], "repo-b");
    assert_eq!(status_b["workspace"]["path"], text(&canonical(&repo_b)));
    assert_eq!(
        status_b["continuity_guardian"]["latest_checkpoint_id"],
        second["id"]
    );
    let status_a = json_in(&home, &repo_a, &["status"]);
    assert_eq!(
        status_a["continuity_guardian"]["latest_checkpoint_id"],
        first["id"]
    );

    // The switch handoff from B carries B's recorded task and decision. The
    // ctx binary stands in for an installed Claude Code (`--version` succeeds).
    json_output(&home, &["profile", "add", "Personal", "--agent", "codex"]);
    json_output(&home, &["profile", "add", "Work", "--agent", "claude"]);
    let outcome = successful_json(
        command_in(&home, &repo_b)
            .env(
                "CONTEXTWAKE_CLAUDE_BIN",
                assert_cmd::cargo::cargo_bin!("ctx"),
            )
            .args(["--json", "profile", "use", "work", "--handoff"])
            .output()
            .expect("switch"),
    );
    assert_eq!(outcome["workspace"]["id"], second["workspace_id"]);
    let checkpoint_id = outcome["handoff"]["checkpoint_id"]
        .as_str()
        .expect("switch checkpoint");
    let carried = json_output(&home, &["checkpoint", "show", checkpoint_id]);
    assert_eq!(carried["workspace_id"], second["workspace_id"]);
    assert_eq!(carried["active_task"], "Implement field quoting");
    assert_eq!(carried["decisions"][0], "Use a streaming writer");

    // An explicit --workspace still wins over the current directory.
    let explicit = json_in(
        &home,
        &repo_b,
        &[
            "checkpoint",
            "create",
            "--objective",
            "Back to A",
            "--workspace",
            &text(&repo_a),
        ],
    );
    assert_eq!(explicit["workspace_id"], first["workspace_id"]);

    // Human output names the target workspace and its path.
    let human = stdout(
        &command_in(&home, &repo_b)
            .args(["checkpoint", "create", "--objective", "Named"])
            .output()
            .expect("checkpoint"),
    );
    assert!(human.contains("repo-b"), "{human}");
    assert!(human.contains(&text(&canonical(&repo_b))), "{human}");
}

#[test]
fn home_directory_and_drive_root_require_an_explicit_workspace() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let user_home = root.path().join("user-home");
    let project = root.path().join("project");
    std::fs::create_dir_all(&user_home).expect("user home");
    git_repository(&project);
    let from_home = |args: &[&str]| {
        command_at_home(&home, &user_home, &user_home)
            .arg("--json")
            .args(args)
            .output()
            .expect("run ctx from the home directory")
    };

    let refused = json_error(&from_home(&["checkpoint", "create", "--objective", "Lost"]));
    assert_eq!(refused["code"], "CWK-WORKSPACE-NOT-FOUND");
    let message = refused["message"].as_str().expect("message");
    assert!(message.contains("home directory"), "{message}");
    assert!(message.contains("--workspace <path>"), "{message}");

    // With an active workspace the guidance names it, and --workspace works.
    let added = json_output(&home, &["workspace", "add", &text(&project)]);
    let refused = json_error(&from_home(&["checkpoint", "create", "--objective", "Lost"]));
    let message = refused["message"].as_str().expect("message");
    assert!(message.contains("project"), "{message}");
    assert!(message.contains(&text(&canonical(&project))), "{message}");
    let explicit = successful_json(from_home(&[
        "checkpoint",
        "create",
        "--objective",
        "Explicit",
        "--workspace",
        &text(&project),
    ]));
    assert_eq!(explicit["workspace_id"], added["id"]);
    assert!(
        json_error(&from_home(&["workspace", "validate"]))["message"]
            .as_str()
            .expect("message")
            .contains("home directory")
    );

    // `ctx status` changes nothing, so it shows the active workspace with a warning.
    let status = successful_json(from_home(&["status"]));
    assert_eq!(status["workspace"]["id"], added["id"]);
    assert!(
        status["warnings"][0]
            .as_str()
            .expect("warning")
            .contains("home directory")
    );

    // Even a registered home directory (older versions registered it implicitly)
    // is never chosen implicitly, neither there nor below it.
    json_output(&home, &["workspace", "add", &text(&user_home)]);
    json_error(&from_home(&["checkpoint", "create", "--objective", "Lost"]));
    let notes = user_home.join("notes");
    std::fs::create_dir(&notes).expect("notes");
    let below = successful_json(
        command_at_home(&home, &user_home, &notes)
            .args(["--json", "checkpoint", "create", "--objective", "Notes"])
            .output()
            .expect("checkpoint below home"),
    );
    let notes_status = json_output(&home, &["status", &text(&notes)]);
    assert_eq!(below["workspace_id"], notes_status["workspace"]["id"]);

    // A drive or filesystem root is refused as well.
    let filesystem_root = canonical(root.path())
        .ancestors()
        .last()
        .expect("filesystem root")
        .to_path_buf();
    let refused = json_error(
        &command_in(&home, &filesystem_root)
            .args(["--json", "checkpoint", "create", "--objective", "Lost"])
            .output()
            .expect("run ctx from a root"),
    );
    assert!(
        refused["message"]
            .as_str()
            .expect("message")
            .contains("root")
    );

    let checkpoints = json_output(&home, &["checkpoint", "list"]);
    let objectives = checkpoints
        .as_array()
        .expect("checkpoints")
        .iter()
        .map(|checkpoint| checkpoint["objective"].as_str().expect("objective"))
        .collect::<Vec<_>>();
    assert_eq!(objectives.len(), 2, "{objectives:?}");
    assert!(!objectives.contains(&"Lost"));
}

#[test]
fn workspace_add_makes_the_workspace_active_and_names_it() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let first = root.path().join("first-project");
    let second = root.path().join("second-project");
    std::fs::create_dir_all(&first).expect("first");
    std::fs::create_dir_all(&second).expect("second");

    for project in [&first, &second] {
        let added = stdout(
            &command(&home)
                .args(["workspace", "add", &text(project)])
                .output()
                .expect("workspace add"),
        );
        assert!(added.contains(&text(&canonical(project))), "{added}");
        assert!(added.contains("active workspace"), "{added}");
    }
    let listed = stdout(
        &command(&home)
            .args(["workspace", "list"])
            .output()
            .expect("workspace list"),
    );
    let active = listed
        .lines()
        .filter(|line| line.starts_with('*'))
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1, "{listed}");
    assert!(active[0].contains("second-project"), "{listed}");
}

#[test]
fn doctor_and_config_repair_run_when_the_configuration_cannot_be_loaded() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join("config")).expect("config directory");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(
        home.join("config/config.toml"),
        "schema_version = 1\ngit_timeout_ms = 50\n",
    )
    .expect("invalid config");

    let doctor = command(&home)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    assert_eq!(doctor.status.code(), Some(1));
    let report: Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor reports instead of aborting");
    let configuration = check(&report, "Configuration");
    assert_eq!(configuration["status"], "fail");
    assert!(
        configuration["summary"]
            .as_str()
            .expect("summary")
            .contains("git_timeout_ms")
    );
    assert_eq!(check(&report, "State database")["status"], "pass");

    // Other commands report the reason and point to doctor, which now runs.
    let status = command(&home)
        .args(["status", &text(&workspace)])
        .output()
        .expect("status");
    assert!(!status.status.success());
    let error = String::from_utf8_lossy(&status.stderr);
    assert!(error.contains("git_timeout_ms"), "{error}");
    assert!(error.contains("ctx doctor"), "{error}");

    // Version, completion and the configuration commands need no valid state.
    assert!(
        stdout(&command(&home).arg("version").output().expect("version"))
            .contains(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        command(&home)
            .args(["completion", "bash"])
            .output()
            .expect("completion")
            .status
            .success()
    );
    assert!(
        stdout(
            &command(&home)
                .args(["config", "path"])
                .output()
                .expect("config path")
        )
        .contains("config.toml")
    );
    let validate = command(&home)
        .args(["config", "validate"])
        .output()
        .expect("config validate");
    assert!(!validate.status.success());
    assert!(String::from_utf8_lossy(&validate.stderr).contains("git_timeout_ms"));

    // The repair command the error points to works and keeps other settings.
    let repaired = json_output(&home, &["config", "set", "--git-timeout-ms", "2000"]);
    assert_eq!(repaired["git_timeout_ms"], 2000);
    assert!(
        command(&home)
            .args(["config", "validate"])
            .output()
            .expect("config validate")
            .status
            .success()
    );
    json_output(&home, &["status", &text(&workspace)]);
}

#[test]
fn doctor_reports_a_state_database_this_build_cannot_open() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    std::fs::create_dir_all(home.join("data")).expect("data directory");
    let connection = rusqlite::Connection::open(home.join("data/state.sqlite3")).expect("db");
    connection
        .execute_batch(
            "CREATE TABLE schema_meta(version INTEGER NOT NULL);
             INSERT INTO schema_meta(version) VALUES (99);",
        )
        .expect("newer schema");
    drop(connection);

    let doctor = command(&home)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    assert_eq!(doctor.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor report");
    let database = check(&report, "State database");
    assert_eq!(database["status"], "fail");
    assert!(
        database["summary"]
            .as_str()
            .expect("summary")
            .contains("not supported")
    );
    assert_eq!(check(&report, "Configuration")["status"], "pass");
    // The configuration commands do not need the database.
    json_output(&home, &["config", "show"]);
    assert!(
        command(&home)
            .args(["config", "validate"])
            .output()
            .expect("config validate")
            .status
            .success()
    );
}

#[test]
fn legacy_alias_uses_its_own_name_for_version_and_completions() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let alias = |binary: &Path, args: &[&str]| {
        let mut command = assert_cmd::Command::new(binary);
        command.env("CONTEXTWAKE_HOME", &home).args(args);
        stdout(&command.output().expect("run alias"))
    };
    let ctxwake = assert_cmd::cargo::cargo_bin!("ctxwake");
    assert!(alias(ctxwake, &["version"]).starts_with("ctxwake "));
    let completion = alias(ctxwake, &["completion", "bash"]);
    assert!(completion.contains("_ctxwake"), "{completion}");
    let ctx = assert_cmd::cargo::cargo_bin!("ctx");
    assert!(alias(ctx, &["version"]).starts_with("ctx "));
    assert!(!alias(ctx, &["completion", "bash"]).contains("ctxwake"));

    // Windows file names are case-insensitive, so CTXWAKE.EXE is the alias too.
    #[cfg(windows)]
    {
        let upper = root.path().join("CTXWAKE.EXE");
        std::fs::copy(ctxwake, &upper).expect("copy alias");
        assert!(alias(&upper, &["version"]).starts_with("ctxwake "));
        assert!(alias(&upper, &["--help"]).contains("Usage: ctxwake"));
    }
}

#[test]
fn workspace_paths_are_terminal_safe_in_human_output() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    json_output(&home, &["workspace", "list"]);
    let store = contextwake::store::Store::open(home.join("data/state.sqlite3")).expect("store");
    let now = chrono::Utc::now();
    store
        .upsert_workspace(&contextwake::model::Workspace {
            id: uuid::Uuid::new_v4(),
            path: PathBuf::from("/srv/repo\u{1b}]0;pwned\u{7}\u{1b}[2J\nActive profile  Admin"),
            display_name: "repo\u{1b}[31m".into(),
            trust_state: contextwake::model::TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        })
        .expect("hostile workspace row");

    let listed = stdout(
        &command(&home)
            .args(["workspace", "list"])
            .output()
            .expect("workspace list"),
    );
    assert!(!listed.contains('\u{1b}'), "{listed:?}");
    assert!(!listed.contains('\u{7}'), "{listed:?}");
    assert_eq!(listed.lines().count(), 1, "{listed:?}");
}

#[cfg(unix)]
#[test]
fn hostile_directory_names_do_not_reach_the_terminal() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let hostile = root.path().join("repo\u{1b}]0;pwned\u{7}\u{1b}[2J");
    std::fs::create_dir(&hostile).expect("hostile directory");
    for args in [
        vec!["status".to_string(), text(&hostile)],
        vec!["workspace".into(), "add".into(), text(&hostile)],
        vec!["workspace".into(), "list".into()],
    ] {
        let output = stdout(&command(&home).args(&args).output().expect("run ctx"));
        assert!(!output.contains('\u{1b}'), "{args:?}: {output:?}");
        assert!(!output.contains('\u{7}'), "{args:?}: {output:?}");
    }
}

#[test]
fn session_sync_counts_only_workspace_sessions_toward_max_count() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let workspace = root.path().join("csv-export");
    let other = root.path().join("other-project");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&other).expect("other workspace");
    let profile = json_output(
        &home,
        &["profile", "add", "Copilot", "--agent", "github-copilot"],
    );
    let agent_home = PathBuf::from(profile["agent_home"].as_str().expect("agent home"));
    let write_session = |id: &str, cwd: &Path, age: Duration| {
        let directory = agent_home.join("session-state").join(id);
        std::fs::create_dir_all(&directory).expect("session directory");
        let event = serde_json::json!({
            "type": "session.start",
            "data": {"sessionId": id, "startTime": "2026-01-01T00:00:00Z", "context": {"cwd": cwd}}
        });
        let path = directory.join("events.jsonl");
        std::fs::write(&path, format!("{event}\n")).expect("session events");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("events file")
            .set_modified(SystemTime::now() - age)
            .expect("session age");
    };
    // The other project's sessions are the most recent ones overall.
    write_session(
        "csv-older",
        &canonical(&workspace),
        Duration::from_secs(3 * 3600),
    );
    write_session(
        "csv-oldest",
        &canonical(&workspace),
        Duration::from_secs(4 * 3600),
    );
    write_session("other-newest", &canonical(&other), Duration::from_secs(60));
    write_session("other-newer", &canonical(&other), Duration::from_secs(120));

    let synced = json_output(
        &home,
        &[
            "session",
            "sync",
            "--workspace",
            &text(&workspace),
            "--max-count",
            "1",
        ],
    );
    assert_eq!(synced["matched"], 1, "{synced}");
    let sessions = json_output(&home, &["session", "list"]);
    let sessions = sessions.as_array().expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["provider_session_id"], "csv-older");
}

#[test]
fn another_profile_for_an_agent_without_identity_isolation_needs_acknowledgement() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    json_output(&home, &["profile", "add", "KiroWork", "--agent", "kiro"]);

    let refused = json_error(
        &command(&home)
            .args([
                "--json",
                "profile",
                "add",
                "KiroPersonal",
                "--agent",
                "kiro",
            ])
            .output()
            .expect("second Kiro profile"),
    );
    assert_eq!(refused["code"], "CWK-AGENT-CAPABILITY-UNAVAILABLE");
    let message = refused["message"].as_str().expect("message");
    assert!(message.contains("--allow-shared-identity"), "{message}");
    assert!(message.contains("KiroWork"), "{message}");
    assert_eq!(
        std::fs::read_dir(home.join("data/agent-homes/kiro"))
            .expect("agent homes")
            .count(),
        1,
        "a refused profile must not create an agent home"
    );

    let acknowledged = command(&home)
        .args([
            "--json",
            "profile",
            "add",
            "KiroPersonal",
            "--agent",
            "kiro",
            "--allow-shared-identity",
        ])
        .output()
        .expect("acknowledged Kiro profile");
    assert!(acknowledged.status.success());
    assert!(String::from_utf8_lossy(&acknowledged.stderr).contains("share one sign-in"));
    assert_eq!(
        json_output(&home, &["profile", "list"])
            .as_array()
            .expect("profiles")
            .len(),
        2
    );
    // Signing in warns before the provider login starts (it fails here because
    // Kiro CLI is not installed in the test environment).
    let login = command(&home)
        .args(["profile", "login", "kiropersonal"])
        .output()
        .expect("login attempt");
    assert!(String::from_utf8_lossy(&login.stderr).contains("share one sign-in"));

    // Agents whose profile homes isolate the identity are unaffected.
    json_output(&home, &["profile", "add", "CodexA", "--agent", "codex"]);
    json_output(&home, &["profile", "add", "CodexB", "--agent", "codex"]);
}

#[test]
fn failed_switch_does_not_register_or_activate_the_workspace() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let other = root.path().join("other");
    std::fs::create_dir_all(&other).expect("other workspace");
    json_output(&home, &["profile", "add", "Personal", "--agent", "codex"]);
    json_output(&home, &["profile", "add", "Work", "--agent", "claude"]);

    let typo = json_error(
        &command(&home)
            .args([
                "--json",
                "profile",
                "use",
                "typo",
                "--handoff",
                "--workspace",
                &text(&other),
            ])
            .output()
            .expect("switch to a mistyped profile"),
    );
    assert_eq!(typo["code"], "CWK-PROFILE-NOT-FOUND");
    let unavailable = command(&home)
        .args([
            "profile",
            "use",
            "work",
            "--handoff",
            "--workspace",
            &text(&other),
        ])
        .output()
        .expect("switch to an unavailable agent");
    assert!(!unavailable.status.success());
    assert!(
        json_output(&home, &["workspace", "list"])
            .as_array()
            .expect("workspaces")
            .is_empty(),
        "a failed switch must not register or activate its workspace"
    );
    assert!(
        json_output(&home, &["checkpoint", "list"])
            .as_array()
            .expect("checkpoints")
            .is_empty()
    );
}

#[test]
fn invalid_trusted_project_configuration_does_not_block_status_or_checkpoints() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".contextwake")).expect("project");
    let config = project.join(".contextwake/project.toml");
    std::fs::write(
        &config,
        "schema_version = 1\n[[validation]]\nexecutable = \"git\"\nargs = [\"--version\"]\n",
    )
    .expect("project config");
    json_output(&home, &["workspace", "add", &text(&project), "--trust"]);
    // A newer ContextWake build or a merge leaves a key this build rejects.
    std::fs::write(&config, "schema_version = 1\nfuture_setting = true\n").expect("newer config");

    let status = json_output(&home, &["status", &text(&project)]);
    assert!(
        status["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("future_setting"))),
        "{status}"
    );
    let created = command(&home)
        .args([
            "--json",
            "checkpoint",
            "create",
            "--objective",
            "Keep going",
            "--workspace",
            &text(&project),
        ])
        .output()
        .expect("checkpoint");
    assert!(String::from_utf8_lossy(&created.stderr).contains("Warning"));
    let checkpoint = successful_json(created);
    assert!(checkpoint["project_instructions"].is_null());

    // Explicit validation still reports the invalid configuration.
    let validation = json_error(
        &command(&home)
            .args([
                "--json",
                "workspace",
                "validate",
                "--workspace",
                &text(&project),
            ])
            .output()
            .expect("validate"),
    );
    assert!(
        validation["message"]
            .as_str()
            .expect("message")
            .contains("future_setting")
    );
}

#[test]
fn a_missing_active_workspace_does_not_block_other_workspaces() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let stale = root.path().join("stale-project");
    let current = root.path().join("current-project");
    std::fs::create_dir_all(&stale).expect("stale project");
    git_repository(&current);
    let added = json_output(&home, &["workspace", "add", &text(&stale)]);
    std::fs::remove_dir_all(&stale).expect("delete the active project");

    let report: Value = serde_json::from_slice(
        &command(&home)
            .args(["--json", "doctor"])
            .output()
            .expect("doctor")
            .stdout,
    )
    .expect("doctor report");
    let active = check(&report, "Active workspace");
    assert_eq!(active["status"], "warning");
    assert!(
        active["action"]
            .as_str()
            .expect("action")
            .contains("ctx workspace remove")
    );

    let reopened = json_error(
        &command(&home)
            .args([
                "--json",
                "workspace",
                "open",
                added["id"].as_str().expect("id"),
            ])
            .output()
            .expect("open stale workspace"),
    );
    assert_eq!(reopened["code"], "CWK-WORKSPACE-NOT-FOUND");
    assert!(
        reopened["message"]
            .as_str()
            .expect("message")
            .contains("ctx workspace remove")
    );

    let checkpoint = json_in(
        &home,
        &current,
        &["checkpoint", "create", "--objective", "Continue"],
    );
    let status = json_in(&home, &current, &["status"]);
    assert_eq!(checkpoint["workspace_id"], status["workspace"]["id"]);
    assert_eq!(status["workspace"]["display_name"], "current-project");
}

#[test]
fn trusted_project_preferences_are_stored_when_a_known_workspace_is_trusted() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".contextwake")).expect("project");
    // `ctx status` (and the TUI) register a directory as untrusted first.
    let first = json_output(&home, &["status", &text(&project)]);
    std::fs::write(
        project.join(".contextwake/project.toml"),
        "schema_version = 1\npreferred_agent = \"codex\"\npreferred_model_provider = \"openai\"\npreferred_model = \"gpt-5.3-codex\"\n",
    )
    .expect("project config");

    let trusted = json_output(&home, &["workspace", "add", &text(&project), "--trust"]);
    assert_eq!(trusted["id"], first["workspace"]["id"]);
    let shown = json_output(
        &home,
        &["workspace", "show", trusted["id"].as_str().expect("id")],
    );
    assert_eq!(shown["trust_state"], "trusted");
    assert_eq!(shown["preferred_agent_id"], "codex");
    assert_eq!(shown["preferred_model_provider_id"], "openai");
    assert_eq!(shown["preferred_model"], "gpt-5.3-codex");
}

#[test]
fn an_unreadable_checkpoint_degrades_status_instead_of_failing_it() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).expect("project");
    let checkpoint = json_output(
        &home,
        &[
            "checkpoint",
            "create",
            "--objective",
            "Before the edit",
            "--workspace",
            &text(&project),
        ],
    );
    // The documented, inspectable checkpoint file is edited by hand.
    std::fs::write(
        home.join("data/checkpoints")
            .join(format!("{}.json", checkpoint["id"].as_str().expect("id"))),
        "{ hand-edited",
    )
    .expect("corrupt checkpoint");

    let status = json_output(&home, &["status", &text(&project)]);
    assert!(
        status["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("checkpoints could not be read"))),
        "{status}"
    );
}

#[test]
fn ordinary_directories_stay_usable_without_git() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("state");
    let project = root.path().join("notes");
    let empty_path = root.path().join("empty-path");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&empty_path).expect("empty PATH directory");
    let without_git = |args: &[&str]| {
        successful_json(
            command(&home)
                .env("PATH", &empty_path)
                .arg("--json")
                .args(args)
                .output()
                .expect("run ctx without Git"),
        )
    };

    let added = without_git(&["workspace", "add", &text(&project)]);
    assert_eq!(added["path"], text(&canonical(&project)));
    let status = without_git(&["status", &text(&project)]);
    assert_eq!(status["workspace"]["id"], added["id"]);
    assert!(status["git"].is_null());
}
