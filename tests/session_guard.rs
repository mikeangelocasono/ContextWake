use std::time::Duration;

use agentdeck::app::Application;
use agentdeck::config::AppConfig;
use agentdeck::error::AgentDeckError;
use agentdeck::git::GitClient;
use agentdeck::model::{ContinuityKind, ResumeCapability, Session, TrustState, Workspace};
use agentdeck::paths::AppPaths;
use agentdeck::provider::AgentRegistry;
use agentdeck::store::Store;
use chrono::Utc;
use uuid::Uuid;

#[test]
fn handoff_only_local_session_is_never_sent_to_native_resume() {
    let root = tempfile::tempdir().expect("tempdir");
    let paths = AppPaths::from_root(root.path().join("home"));
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let git = GitClient::with_executable("agentdeck-test-missing-git", Duration::from_millis(100));
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
        Err(AgentDeckError::CapabilityUnavailable(_))
    ));
}
