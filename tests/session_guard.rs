use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use contextwake::app::Application;
use contextwake::config::AppConfig;
use contextwake::error::ContextWakeError;
use contextwake::git::GitClient;
use contextwake::model::{ContinuityKind, ResumeCapability, Session, TrustState, Workspace};
use contextwake::paths::AppPaths;
use contextwake::provider::AgentRegistry;
use contextwake::store::Store;
use serde_json::Value;
use uuid::Uuid;

/// `ctx` with isolated state, in `directory`, where every agent executable is
/// missing: a resume that passes the guards fails to start instead of running
/// an installed coding agent.
fn ctx(state: &Path, directory: &Path) -> assert_cmd::Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("ctx");
    command
        .env("CONTEXTWAKE_HOME", state)
        .current_dir(directory);
    for agent in [
        "CODEX", "CLAUDE", "COPILOT", "CURSOR", "GEMINI", "GROK", "KIMI", "KIRO", "OPENCODE",
    ] {
        command.env(
            format!("CONTEXTWAKE_{agent}_BIN"),
            directory
                .join("missing-agents")
                .join(agent.to_ascii_lowercase()),
        );
    }
    command
}

fn resume_error(state: &Path, directory: &Path, reference: &str) -> Value {
    let output = ctx(state, directory)
        .args(["--json", "session", "resume", reference])
        .output()
        .expect("run ctx session resume");
    assert!(!output.status.success(), "resume unexpectedly succeeded");
    serde_json::from_slice(&output.stderr).expect("structured JSON error")
}

fn workspace_row(path: &Path) -> Workspace {
    let now = Utc::now();
    Workspace {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        display_name: "workspace".into(),
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root: None,
        created_at: now,
        last_opened_at: now,
    }
}

#[test]
fn unindexed_reference_is_not_resumed_without_verified_native_resume() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let added = ctx(&state, &project)
        .args(["profile", "add", "Kimi", "--agent", "kimi"])
        .output()
        .expect("add Kimi profile");
    assert!(added.status.success());

    // Kimi's native resume is only partially verified, so a raw provider ID
    // that ContextWake never indexed must not reach the agent.
    let error = resume_error(&state, &project, "0f3c2b1a-unindexed");
    assert_eq!(error["code"], "CWK-AGENT-CAPABILITY-UNAVAILABLE");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("only sessions indexed"), "{message}");
    assert!(message.contains("not attempted"), "{message}");
    assert!(!message.contains("could not start"), "{message}");
}

#[test]
fn unindexed_reference_reaches_an_agent_with_verified_native_resume() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let added = ctx(&state, &project)
        .args(["profile", "add", "Codex", "--agent", "codex"])
        .output()
        .expect("add Codex profile");
    assert!(added.status.success());

    // The guard passes; only the missing agent executable stops the launch.
    let error = resume_error(&state, &project, "019abc-raw-session");
    let message = error["message"].as_str().expect("message");
    assert!(!message.contains("only sessions indexed"), "{message}");
    assert!(!message.contains("not attempted"), "{message}");
}

#[test]
fn indexed_session_is_not_resumed_in_a_missing_workspace_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let project = root.path().join("renamed-project");
    std::fs::create_dir(&project).expect("project");
    let added = ctx(&state, root.path())
        .args(["profile", "add", "Personal", "--agent", "codex"])
        .output()
        .expect("add profile");
    assert!(added.status.success());
    let store = Store::open(state.join("data/state.sqlite3")).expect("store");
    let personal = store
        .active_profile()
        .expect("active")
        .expect("active profile");
    let workspace = workspace_row(&project);
    store.upsert_workspace(&workspace).expect("workspace");
    let now = Utc::now();
    let session = Session {
        id: Uuid::new_v4(),
        provider_session_id: Some("019abc-session".into()),
        title: None,
        workspace_id: workspace.id,
        profile_id: Some(personal.id),
        agent_id: "codex".into(),
        model_provider_id: None,
        model: None,
        started_at: now,
        last_seen_at: now,
        resume_capability: ResumeCapability::Native,
        archived: false,
        continuity: ContinuityKind::NativeResume,
    };
    store.insert_session(&session).expect("session");
    std::fs::remove_dir(&project).expect("rename the project away");

    let error = resume_error(&state, root.path(), &session.id.to_string());
    assert_eq!(error["code"], "CWK-WORKSPACE-NOT-FOUND");
    let message = error["message"].as_str().expect("message");
    assert!(
        message.contains(&format!("ctx workspace remove {} --yes", workspace.id)),
        "{message}"
    );
}

/// Workspaces stored by older builds in the Windows verbatim form are handed to
/// agents (which key their session stores by the working directory string) in
/// the conventional form.
#[cfg(windows)]
#[test]
fn stored_verbatim_workspace_path_reaches_the_agent_in_conventional_form() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let added = ctx(&state, root.path())
        .args(["profile", "add", "Personal", "--agent", "codex"])
        .output()
        .expect("add profile");
    assert!(added.status.success());
    let store = Store::open(state.join("data/state.sqlite3")).expect("store");
    let personal = store
        .active_profile()
        .expect("active")
        .expect("active profile");
    let verbatim = project.canonicalize().expect("verbatim canonical path");
    assert!(verbatim.to_string_lossy().starts_with(r"\\?\"));
    let workspace = workspace_row(&verbatim);
    store.upsert_workspace(&workspace).expect("workspace");
    let now = Utc::now();
    let session = Session {
        id: Uuid::new_v4(),
        provider_session_id: Some("019abc-session".into()),
        title: None,
        workspace_id: workspace.id,
        profile_id: Some(personal.id),
        agent_id: "codex".into(),
        model_provider_id: None,
        model: None,
        started_at: now,
        last_seen_at: now,
        resume_capability: ResumeCapability::Native,
        archived: false,
        continuity: ContinuityKind::NativeResume,
    };
    store.insert_session(&session).expect("session");

    let output = ctx(&state, root.path())
        .args(["session", "resume", &session.id.to_string()])
        .output()
        .expect("resume");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let conventional = contextwake::paths::canonicalize_simplified(&project).expect("canonical");
    assert!(
        stdout.contains(&format!("({})", conventional.display())),
        "{stdout}"
    );
    assert!(!stdout.contains(r"\\?\"), "{stdout}");
}

#[test]
fn session_of_a_removed_profile_is_never_natively_resumed() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let project = root.path().join("project");
    std::fs::create_dir(&project).expect("project");
    for (name, agent) in [("Personal", "codex"), ("Work", "codex")] {
        let added = ctx(&state, &project)
            .args(["profile", "add", name, "--agent", agent])
            .output()
            .expect("add profile");
        assert!(added.status.success());
    }
    let store = Store::open(state.join("data/state.sqlite3")).expect("store");
    let work = store.profile("work").expect("work profile");
    let workspace = workspace_row(&project);
    store.upsert_workspace(&workspace).expect("workspace");
    let now = Utc::now();
    let session = Session {
        id: Uuid::new_v4(),
        provider_session_id: Some("019abc-work-session".into()),
        title: None,
        workspace_id: workspace.id,
        profile_id: Some(work.id),
        agent_id: "codex".into(),
        model_provider_id: None,
        model: None,
        started_at: now,
        last_seen_at: now,
        resume_capability: ResumeCapability::Native,
        archived: false,
        continuity: ContinuityKind::NativeResume,
    };
    store.insert_session(&session).expect("session");
    // Removing Work clears the session's profile (ON DELETE SET NULL) while
    // Personal, which uses the same agent, stays active.
    store.remove_profile("work").expect("remove work");
    assert!(
        store
            .session(&session.id.to_string())
            .expect("session")
            .profile_id
            .is_none()
    );

    let error = resume_error(&state, &project, &session.id.to_string());
    assert_eq!(error["code"], "CWK-AGENT-ERROR");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("profile was removed"), "{message}");
    assert!(message.contains("not attempted"), "{message}");
}

#[test]
fn handoff_only_local_session_is_never_sent_to_native_resume() {
    let root = tempfile::tempdir().expect("tempdir");
    let paths = AppPaths::from_root(root.path().join("home"));
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let git =
        GitClient::with_executable("contextwake-test-missing-git", Duration::from_millis(100));
    let app = Application {
        paths,
        config: AppConfig::default(),
        store: store.clone(),
        git: git.clone(),
        agents: AgentRegistry::discover(),
    };
    app.create_profile("Personal", None, None).expect("profile");
    let workspace_directory = root.path().join("workspace");
    std::fs::create_dir(&workspace_directory).expect("workspace directory");
    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: workspace_directory,
        display_name: "workspace".into(),
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root: None,
        created_at: now,
        last_opened_at: now,
    };
    store.upsert_workspace(&workspace).expect("store workspace");
    let session = Session {
        id: Uuid::new_v4(),
        provider_session_id: None,
        title: None,
        workspace_id: workspace.id,
        profile_id: store
            .active_profile()
            .expect("active")
            .map(|profile| profile.id),
        agent_id: "codex".into(),
        model_provider_id: Some("openai".into()),
        model: None,
        started_at: now,
        last_seen_at: now,
        resume_capability: ResumeCapability::HandoffOnly,
        archived: false,
        continuity: ContinuityKind::RestoredFromHandoff,
    };
    store.insert_session(&session).expect("session");

    let result = app.resume_session(&session.id.to_string());
    assert!(matches!(
        result,
        Err(ContextWakeError::CapabilityUnavailable(_))
    ));
}
