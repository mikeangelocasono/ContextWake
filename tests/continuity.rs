use std::time::Duration;

use agentdeck::checkpoint::CheckpointService;
use agentdeck::continuity::switch_profile;
use agentdeck::git::GitClient;
use agentdeck::handoff::HandoffService;
use agentdeck::model::{AuthState, Profile, TrustState, Workspace};
use agentdeck::paths::AppPaths;
use agentdeck::store::Store;
use chrono::Utc;
use uuid::Uuid;

fn profile(paths: &AppPaths, name: &str) -> Profile {
    let now = Utc::now();
    let id = Uuid::new_v4();
    Profile {
        id,
        name: name.into(),
        display_name: name.into(),
        agent_id: "codex".into(),
        model_provider_id: Some("openai".into()),
        model_preference: None,
        description: None,
        agent_home: paths.agent_home("codex", &id),
        account_fingerprint: None,
        auth_state: AuthState::SignedOut,
        created_at: now,
        updated_at: now,
        last_used_at: None,
    }
}

#[test]
fn failed_handoff_keeps_previous_profile_active() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path());
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let personal = profile(&paths, "personal");
    let work = profile(&paths, "work");
    store.insert_profile(&personal).expect("personal");
    store.insert_profile(&work).expect("work");
    store.set_active_profile(personal.id).expect("activate");

    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: root.path().join("workspace"),
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
    std::fs::create_dir_all(&workspace.path).expect("workspace");
    store.upsert_workspace(&workspace).expect("register");

    let broken_git =
        GitClient::with_executable("agentdeck-test-missing-git", Duration::from_millis(100));
    let checkpoints = CheckpointService::new(store.clone(), paths.clone(), broken_git);
    let handoffs = HandoffService::new(store.clone(), paths);
    let result = switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "work",
        Some(&workspace),
        true,
        Some("Continue safely".into()),
    );

    assert!(result.is_err());
    assert_eq!(
        store.active_profile().expect("active").expect("profile").id,
        personal.id
    );
    assert!(
        store
            .list_checkpoint_paths()
            .expect("checkpoints")
            .is_empty()
    );
}

#[test]
fn successful_switch_records_handoff_without_claiming_resume() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path());
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let personal = profile(&paths, "personal");
    let work = profile(&paths, "work");
    store.insert_profile(&personal).expect("personal");
    store.insert_profile(&work).expect("work");
    store.set_active_profile(personal.id).expect("activate");

    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: root.path().join("workspace"),
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
    std::fs::create_dir_all(&workspace.path).expect("workspace");
    store.upsert_workspace(&workspace).expect("register");

    let checkpoints = CheckpointService::new(
        store.clone(),
        paths.clone(),
        GitClient::new(Duration::from_secs(5)),
    );
    let handoffs = HandoffService::new(store.clone(), paths);
    let outcome = switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "work",
        Some(&workspace),
        true,
        Some("Continue safely".into()),
    )
    .expect("switch");

    assert_eq!(outcome.active.id, work.id);
    assert!(outcome.handoff.is_some());
    assert!(
        outcome
            .message
            .contains("no native session resume was claimed")
    );
    assert_eq!(
        store.active_profile().expect("active").expect("profile").id,
        work.id
    );
}
