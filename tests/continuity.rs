use std::fs;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use contextwake::checkpoint::{CheckpointInput, CheckpointService};
use contextwake::continuity::switch_profile;
use contextwake::git::GitClient;
use contextwake::handoff::HandoffService;
use contextwake::model::{AuthState, Profile, TrustState, Workspace};
use contextwake::paths::AppPaths;
use contextwake::provider::AgentRegistry;
use contextwake::store::Store;
use uuid::Uuid;

fn profile(paths: &AppPaths, name: &str) -> Profile {
    profile_for_agent(paths, name, "codex")
}

fn profile_for_agent(paths: &AppPaths, name: &str, agent_id: &str) -> Profile {
    let now = Utc::now();
    let id = Uuid::new_v4();
    Profile {
        id,
        name: name.into(),
        display_name: name.into(),
        agent_id: agent_id.into(),
        model_provider_id: None,
        model_preference: None,
        description: None,
        agent_home: paths.agent_home(agent_id, &id),
        account_fingerprint: None,
        auth_state: AuthState::SignedOut,
        created_at: now,
        updated_at: now,
        last_used_at: None,
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("Git must be installed for continuity integration tests");
    assert!(status.success(), "git {args:?} failed");
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
        GitClient::with_executable("contextwake-test-missing-git", Duration::from_millis(100));
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
    let source = checkpoints
        .create(
            &workspace,
            Some(&personal),
            None,
            CheckpointInput {
                objective: "Implement checked division".into(),
                active_task: Some("Handle division by zero".into()),
                completed: vec!["Addition".into(), "Subtraction".into()],
                decisions: vec!["Return Result instead of panicking".into()],
                pending_tasks: vec!["Add division tests".into()],
                known_issues: vec!["Division is not implemented".into()],
                user_notes: Some("Run cargo test before handoff".into()),
                validation_results: Vec::new(),
            },
        )
        .expect("source checkpoint");
    let outcome = switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "work",
        Some(&workspace),
        true,
        None,
    )
    .expect("switch");

    assert_eq!(outcome.active.id, work.id);
    assert!(outcome.handoff.is_some());
    let manifest = handoffs
        .manifest(&outcome.handoff.as_ref().expect("handoff").id.to_string())
        .expect("manifest");
    assert_eq!(manifest.schema_version, "1.2.0");
    assert_eq!(
        manifest.continuity_mode.as_deref(),
        Some("portable_handoff")
    );
    assert_eq!(
        manifest
            .destination
            .as_ref()
            .expect("switch destination")
            .agent_id,
        work.agent_id
    );
    let refreshed = checkpoints
        .read(
            &outcome
                .handoff
                .as_ref()
                .expect("handoff")
                .checkpoint_id
                .to_string(),
        )
        .expect("switch checkpoint");
    assert_ne!(refreshed.id, source.id);
    assert_eq!(refreshed.objective, source.objective);
    assert_eq!(refreshed.active_task, source.active_task);
    assert_eq!(refreshed.completed, source.completed);
    assert_eq!(refreshed.decisions, source.decisions);
    assert!(refreshed.pending_tasks.starts_with(&source.pending_tasks));
    assert_eq!(
        refreshed.pending_tasks.len(),
        source.pending_tasks.len() + 1
    );
    assert_eq!(
        refreshed.pending_tasks.last().map(String::as_str),
        Some("Review this handoff after activating profile work")
    );
    assert_eq!(refreshed.known_issues, source.known_issues);
    assert_eq!(refreshed.user_notes, source.user_notes);
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

#[test]
fn repeated_switch_replaces_stale_target_review_task() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path());
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let personal = profile(&paths, "personal");
    let work = profile(&paths, "work");
    let client = profile(&paths, "client");
    store.insert_profile(&personal).expect("personal");
    store.insert_profile(&work).expect("work");
    store.insert_profile(&client).expect("client");
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
    checkpoints
        .create(
            &workspace,
            Some(&personal),
            None,
            CheckpointInput {
                objective: "Continue calculator work".into(),
                pending_tasks: vec!["Implement division".into()],
                ..CheckpointInput::default()
            },
        )
        .expect("source checkpoint");

    switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "work",
        Some(&workspace),
        true,
        None,
    )
    .expect("first switch");
    let outcome = switch_profile(
        &store,
        &checkpoints,
        &handoffs,
        "client",
        Some(&workspace),
        true,
        None,
    )
    .expect("second switch");
    let refreshed = checkpoints
        .read(
            &outcome
                .handoff
                .as_ref()
                .expect("handoff")
                .checkpoint_id
                .to_string(),
        )
        .expect("checkpoint");

    assert_eq!(
        refreshed.pending_tasks,
        vec![
            "Implement division",
            "Review this handoff after activating profile client",
        ]
    );
}

#[test]
fn nine_agent_portable_handoff_matrix_preserves_the_csv_fixture() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path().join("state"));
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let agents = [
        ("codex", "codex"),
        ("github-copilot", "copilot"),
        ("claude", "claude"),
        ("cursor", "cursor"),
        ("opencode", "opencode"),
        ("kimi", "kimi"),
        ("grok", "grok"),
    ];
    let profiles = agents
        .iter()
        .map(|(agent, name)| profile_for_agent(&paths, name, agent))
        .collect::<Vec<_>>();
    for profile in &profiles {
        store.insert_profile(profile).expect("profile");
    }
    store
        .set_active_profile(profiles[0].id)
        .expect("activate Codex");

    let repository = root.path().join("contextwake-continuity-fixture");
    fs::create_dir_all(&repository).expect("repository");
    git(&repository, &["init", "-b", "main"]);
    git(
        &repository,
        &["config", "user.email", "continuity@example.invalid"],
    );
    git(
        &repository,
        &["config", "user.name", "ContextWake Continuity"],
    );
    fs::write(repository.join("export.rs"), "pub fn export_json() {}\n").expect("seed export");
    fs::write(repository.join("README.md"), "# Export CLI\n").expect("seed readme");
    git(&repository, &["add", "export.rs", "README.md"]);
    git(&repository, &["commit", "-m", "implement JSON export"]);
    fs::write(
        repository.join("export.rs"),
        "pub fn export_json() {}\npub fn render_table() {}\n",
    )
    .expect("staged change");
    git(&repository, &["add", "export.rs"]);
    fs::write(
        repository.join("README.md"),
        "# Export CLI\n\nCSV export is in progress.\n",
    )
    .expect("unstaged change");
    fs::write(
        repository.join("csv.rs"),
        "// TODO: stream rows and quote comma-containing fields\n",
    )
    .expect("untracked change");

    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: repository.clone(),
        display_name: "contextwake-continuity-fixture".into(),
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root: Some(repository.clone()),
        created_at: now,
        last_opened_at: now,
    };
    store.upsert_workspace(&workspace).expect("workspace");
    let checkpoints = CheckpointService::new(
        store.clone(),
        paths.clone(),
        GitClient::new(Duration::from_secs(5)),
    );
    let handoffs = HandoffService::new(store.clone(), paths);
    checkpoints
        .create(
            &workspace,
            Some(&profiles[0]),
            None,
            CheckpointInput {
                objective: "Add CSV export to a CLI application".into(),
                active_task: Some("Implement CSV writer".into()),
                completed: vec!["JSON export".into(), "Table rendering".into()],
                decisions: vec!["Use streaming output".into()],
                pending_tasks: vec!["Quoting".into(), "Tests".into(), "Docs".into()],
                known_issues: vec!["Fields containing commas need quoting".into()],
                user_notes: Some("Constraint: no new runtime dependency".into()),
                validation_results: Vec::new(),
            },
        )
        .expect("fixture checkpoint");

    for destination_index in 1..=profiles.len() {
        let source = &profiles[destination_index - 1];
        let destination = &profiles[destination_index % profiles.len()];
        let outcome = switch_profile(
            &store,
            &checkpoints,
            &handoffs,
            &destination.name,
            Some(&workspace),
            true,
            None,
        )
        .expect("portable switch");
        let handoff = outcome.handoff.expect("handoff");
        let manifest = handoffs
            .manifest(&handoff.id.to_string())
            .expect("manifest");
        assert_eq!(
            manifest.source.as_ref().expect("source").agent_id,
            source.agent_id
        );
        assert_eq!(
            manifest.destination.as_ref().expect("destination").agent_id,
            destination.agent_id
        );
        assert_eq!(
            manifest.continuity_mode.as_deref(),
            Some("portable_handoff")
        );
        let checkpoint = checkpoints
            .read(&handoff.checkpoint_id.to_string())
            .expect("checkpoint");
        assert_eq!(checkpoint.objective, "Add CSV export to a CLI application");
        assert_eq!(
            checkpoint.active_task.as_deref(),
            Some("Implement CSV writer")
        );
        assert_eq!(checkpoint.completed, ["JSON export", "Table rendering"]);
        assert_eq!(checkpoint.decisions, ["Use streaming output"]);
        assert!(checkpoint.pending_tasks.starts_with(&[
            "Quoting".into(),
            "Tests".into(),
            "Docs".into()
        ]));
        assert_eq!(
            checkpoint.known_issues,
            ["Fields containing commas need quoting"]
        );
        assert_eq!(
            checkpoint.user_notes.as_deref(),
            Some("Constraint: no new runtime dependency")
        );
        let git = checkpoint.git.expect("Git snapshot");
        assert_eq!(git.staged_count, 1);
        assert_eq!(git.unstaged_count, 1);
        assert_eq!(git.untracked_count, 1);
    }

    let registry = AgentRegistry::discover();
    for agent in registry.implemented() {
        assert!(
            agent.capabilities().portable_handoff.is_available(),
            "{} must expose a portable handoff path",
            agent.id()
        );
    }
}
