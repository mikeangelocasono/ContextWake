use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use contextwake::checkpoint::{CheckpointInput, CheckpointService};
use contextwake::continuity::switch_profile;
use contextwake::error::ContextWakeError;
use contextwake::git::GitClient;
use contextwake::handoff::{HandoffProvenance, HandoffService, UNKNOWN_SOURCE_AGENT};
use contextwake::model::{
    AuthState, Checkpoint, HandoffRecord, Profile, RedactionStatus, TrustState, ValidationResult,
    ValidationStatus, Workspace,
};
use contextwake::paths::AppPaths;
use contextwake::provider::AgentRegistry;
use contextwake::security::sha256_bytes;
use contextwake::store::Store;
use serde_json::{Value, json};
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
        home_mode: contextwake::model::ProfileHomeMode::Isolated,
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
    // The workspace directory was removed after registration, so creating the switch
    // checkpoint fails. (A missing Git alone no longer fails: the directory is then an
    // ordinary workspace.)
    store.upsert_workspace(&workspace).expect("register");

    // Make checkpoint persistence fail: the checkpoint directory is replaced by
    // a regular file. (This does not depend on how a missing Git executable is
    // reported, which is allowed to degrade to a non-Git snapshot.)
    std::fs::remove_dir(paths.checkpoints_dir()).expect("remove checkpoint directory");
    std::fs::write(paths.checkpoints_dir(), "not a directory").expect("block checkpoints");
    let checkpoints = CheckpointService::new(
        store.clone(),
        paths.clone(),
        GitClient::new(Duration::from_secs(5)),
    );
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

struct State {
    paths: AppPaths,
    store: Store,
    checkpoints: CheckpointService,
    handoffs: HandoffService,
}

fn state(root: &Path) -> State {
    let paths = AppPaths::from_root(root);
    paths.ensure().expect("paths");
    let store = Store::open(paths.state_db()).expect("store");
    let checkpoints = CheckpointService::new(
        store.clone(),
        paths.clone(),
        GitClient::new(Duration::from_secs(5)),
    );
    let handoffs = HandoffService::new(store.clone(), paths.clone());
    State {
        paths,
        store,
        checkpoints,
        handoffs,
    }
}

fn registered_workspace(store: &Store, path: &Path, git_root: Option<PathBuf>) -> Workspace {
    fs::create_dir_all(path).expect("workspace directory");
    let now = Utc::now();
    let workspace = Workspace {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        display_name: path
            .file_name()
            .expect("workspace name")
            .to_string_lossy()
            .into_owned(),
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root,
        created_at: now,
        last_opened_at: now,
    };
    store
        .upsert_workspace(&workspace)
        .expect("register workspace");
    workspace
}

fn objective(text: &str) -> CheckpointInput {
    CheckpointInput {
        objective: text.into(),
        ..CheckpointInput::default()
    }
}

fn import_fresh(root: &Path, package: &Path) -> Result<HandoffRecord, ContextWakeError> {
    state(&root.join(format!("import-{}", Uuid::new_v4())))
        .handoffs
        .import_directory(package)
}

#[test]
fn profile_less_checkpoint_produces_a_handoff_every_reader_accepts() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    // Fresh install: no profile exists yet, so the checkpoint has no agent.
    let checkpoint = local
        .checkpoints
        .create(&workspace, None, None, objective("Ship CSV export"))
        .expect("checkpoint");
    assert!(checkpoint.agent_id.is_none());

    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");
    let id = record.id.to_string();
    let manifest = local.handoffs.validate(&id).expect("own validator");
    assert_eq!(
        manifest.source.as_ref().expect("source").agent_id,
        UNKNOWN_SOURCE_AGENT
    );
    assert!(
        local
            .handoffs
            .context(&id)
            .expect("preview")
            .contains("Ship CSV export")
    );
    let exported = root.path().join("exported");
    local
        .handoffs
        .export(&id, &exported, false)
        .expect("export");
    let imported = import_fresh(root.path(), &exported).expect("import elsewhere");
    assert_eq!(imported.id, record.id);
}

#[test]
fn switch_after_the_active_profile_was_removed_creates_a_continuable_handoff() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let work = profile(&local.paths, "work");
    let personal = profile(&local.paths, "personal");
    local.store.insert_profile(&work).expect("work");
    local.store.insert_profile(&personal).expect("personal");
    local.store.set_active_profile(work.id).expect("activate");
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    local
        .checkpoints
        .create(
            &workspace,
            Some(&work),
            None,
            objective("Continue CSV export"),
        )
        .expect("checkpoint");
    local.store.remove_profile("work").expect("remove active");
    assert!(local.store.active_profile().expect("active").is_none());

    let outcome = switch_profile(
        &local.store,
        &local.checkpoints,
        &local.handoffs,
        "personal",
        Some(&workspace),
        true,
        None,
    )
    .expect("switch");
    let handoff = outcome.handoff.expect("handoff").id.to_string();
    let manifest = local.handoffs.validate(&handoff).expect("validates");
    assert_eq!(
        manifest.source.expect("source").agent_id,
        UNKNOWN_SOURCE_AGENT
    );
    assert_eq!(
        manifest.destination.expect("destination").agent_id,
        personal.agent_id
    );
    local.handoffs.context(&handoff).expect("preview");
}

#[test]
fn git_summary_is_portable_redacted_and_importable() {
    let root = tempfile::tempdir().expect("root");
    let private_marker = root
        .path()
        .file_name()
        .expect("temporary directory name")
        .to_string_lossy()
        .into_owned();
    let local = state(&root.path().join("state"));
    let owner = profile(&local.paths, "personal");
    local.store.insert_profile(&owner).expect("profile");
    let repository = root.path().join("client-repository");
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
    fs::write(repository.join("README.md"), "# Repo\n").expect("seed");
    git(&repository, &["add", "README.md"]);
    git(
        &repository,
        &["commit", "-m", "Rotate token=abcd1234efgh5678 for staging"],
    );
    fs::write(repository.join("README.md"), "# Repo\n\nDirty\n").expect("dirty");
    let workspace = registered_workspace(&local.store, &repository, Some(repository.clone()));
    let checkpoint = local
        .checkpoints
        .create(&workspace, Some(&owner), None, objective("Continue"))
        .expect("checkpoint");
    // The local checkpoint file redacts the commit subject too, and stores the
    // repository root in its conventional form.
    let stored = fs::read_to_string(
        local
            .store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("indexed path"),
    )
    .expect("stored checkpoint");
    assert!(
        !stored.contains("abcd1234efgh5678"),
        "checkpoint file stores the raw commit subject"
    );
    let snapshot = checkpoint.git.as_ref().expect("git snapshot");
    assert!(
        snapshot
            .last_commit
            .as_deref()
            .expect("subject")
            .contains("[REDACTED]")
    );
    assert!(
        !snapshot
            .repository_root
            .as_deref()
            .expect("repository root")
            .to_string_lossy()
            .starts_with(r"\\?\")
    );
    assert_eq!(snapshot.branch.as_deref(), Some("main"));
    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");

    let summary_text =
        fs::read_to_string(record.storage_path.join("git-summary.json")).expect("summary");
    let summary: Value = serde_json::from_str(&summary_text).expect("summary JSON");
    assert!(summary.get("repository_root").is_none());
    assert!(!summary_text.contains(&private_marker));
    assert!(!summary_text.contains("abcd1234efgh5678"));
    assert!(
        summary["last_commit"]
            .as_str()
            .expect("subject")
            .contains("[REDACTED]")
    );
    assert_eq!(summary["branch"], "main");
    assert_eq!(summary["dirty"], true);

    let exported = root.path().join("exported");
    local
        .handoffs
        .export(&record.id.to_string(), &exported, false)
        .expect("export");
    for entry in fs::read_dir(&exported).expect("exported files") {
        let path = entry.expect("entry").path();
        let text = fs::read_to_string(&path).expect("exported text");
        assert!(
            !text.contains(&private_marker),
            "{} leaks the absolute workspace path",
            path.display()
        );
    }
    import_fresh(root.path(), &exported).expect("ContextWake imports its own export");
}

#[test]
fn large_change_sets_produce_handoffs_within_reader_limits() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    let mut checkpoint = local
        .checkpoints
        .create(&workspace, None, None, objective("Untangle dependencies"))
        .expect("checkpoint");
    // About 20,000 untracked files with typical dependency paths, as in an
    // un-ignored node_modules: well over 1 MiB when listed in full.
    checkpoint.files_changed = (0..20_000)
        .map(|index| {
            PathBuf::from(format!(
                "node_modules/package-{:05}/dist/esm/internal/helpers/module-{index:05}.mjs",
                index / 10
            ))
        })
        .collect();
    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");
    let id = record.id.to_string();
    let size = fs::metadata(record.storage_path.join("context.md"))
        .expect("context")
        .len();
    assert!(size < 1024 * 1024, "context.md is {size} bytes");
    local.handoffs.validate(&id).expect("validate");
    let context = local.handoffs.context(&id).expect("preview");
    assert!(context.contains("more changed files not listed (20000 in total"));
    let exported = root.path().join("exported");
    local
        .handoffs
        .export(&id, &exported, false)
        .expect("export");
    import_fresh(root.path(), &exported).expect("import");
}

#[test]
fn unreadable_checkpoints_are_skipped_reported_and_deletable() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let personal = profile(&local.paths, "personal");
    let work = profile(&local.paths, "work");
    local.store.insert_profile(&personal).expect("personal");
    local.store.insert_profile(&work).expect("work");
    local
        .store
        .set_active_profile(personal.id)
        .expect("activate");
    let alpha = registered_workspace(&local.store, &root.path().join("alpha"), None);
    let beta = registered_workspace(&local.store, &root.path().join("beta"), None);
    let create = |workspace: &Workspace, text: &str| {
        local
            .checkpoints
            .create(workspace, Some(&personal), None, objective(text))
            .expect("checkpoint")
    };
    let path_of = |checkpoint: &Checkpoint| {
        local
            .store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("indexed path")
    };
    let alpha_readable = create(&alpha, "Alpha readable");
    let alpha_missing = create(&alpha, "Alpha missing file");
    let beta_readable = create(&beta, "Beta readable");
    let beta_corrupt = create(&beta, "Beta corrupt file");
    let beta_future = create(&beta, "Beta future schema");
    // A file deleted by hand (or quarantined), a corrupt file, and a file
    // written by a newer build before a downgrade.
    fs::remove_file(path_of(&alpha_missing)).expect("delete file");
    fs::write(path_of(&beta_corrupt), "{ not json").expect("corrupt file");
    let mut future: Value =
        serde_json::from_str(&fs::read_to_string(path_of(&beta_future)).expect("read"))
            .expect("checkpoint JSON");
    future["schema_version"] = json!(4);
    fs::write(
        path_of(&beta_future),
        serde_json::to_vec_pretty(&future).expect("encode"),
    )
    .expect("future schema");

    let readable = local.checkpoints.list().expect("list survives");
    assert_eq!(
        readable.iter().map(|item| item.id).collect::<BTreeSet<_>>(),
        BTreeSet::from([alpha_readable.id, beta_readable.id])
    );
    let listing = local.checkpoints.list_with_unreadable().expect("listing");
    assert_eq!(listing.unreadable.len(), 3);
    let reported = |id: Uuid| {
        listing
            .unreadable
            .iter()
            .find(|entry| entry.id == id)
            .expect("reported")
            .clone()
    };
    let missing = reported(alpha_missing.id);
    assert_eq!(missing.reason, "checkpoint file is missing");
    assert_eq!(missing.objective, "Alpha missing file");
    assert_eq!(
        missing.recovery_command(),
        format!("ctx checkpoint delete {} --yes", alpha_missing.id)
    );
    assert!(
        reported(beta_corrupt.id)
            .reason
            .contains("invalid checkpoint")
    );
    assert!(
        reported(beta_future.id)
            .reason
            .contains("unsupported schema version 4")
    );

    let latest = local
        .checkpoints
        .latest_for_workspace_with_unreadable(alpha.id)
        .expect("latest alpha");
    assert_eq!(
        latest.checkpoint.as_ref().map(|item| item.id),
        Some(alpha_readable.id)
    );
    assert_eq!(
        latest
            .unreadable
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [alpha_missing.id]
    );
    assert_eq!(
        local
            .checkpoints
            .latest_for_workspace(beta.id)
            .expect("latest beta")
            .map(|item| item.id),
        Some(beta_readable.id)
    );
    let error = local
        .checkpoints
        .read(&alpha_missing.id.to_string())
        .expect_err("missing file");
    assert!(matches!(error, ContextWakeError::CheckpointNotFound(_)));
    assert!(error.to_string().contains("ctx checkpoint delete"));

    // Profile switching with a handoff keeps working and is seeded from the
    // newest readable checkpoint.
    let outcome = switch_profile(
        &local.store,
        &local.checkpoints,
        &local.handoffs,
        "work",
        Some(&alpha),
        true,
        None,
    )
    .expect("switch");
    let switched = local
        .checkpoints
        .read(&outcome.handoff.expect("handoff").checkpoint_id.to_string())
        .expect("switch checkpoint");
    assert_eq!(switched.objective, "Alpha readable");

    // `checkpoint delete` cleans up dangling entries and corrupt files.
    local
        .checkpoints
        .delete(&alpha_missing.id.to_string())
        .expect("delete dangling entry");
    let corrupt_path = path_of(&beta_corrupt);
    local
        .checkpoints
        .delete(&beta_corrupt.id.to_string())
        .expect("delete corrupt checkpoint");
    assert!(!corrupt_path.exists());
    let remaining = local.checkpoints.list_with_unreadable().expect("listing");
    assert_eq!(
        remaining
            .unreadable
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [beta_future.id]
    );
    assert!(matches!(
        local.checkpoints.read(&alpha_missing.id.to_string()),
        Err(ContextWakeError::CheckpointNotFound(_))
    ));
}

#[test]
fn status_survives_a_missing_checkpoint_file() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::from_root(root.path().join("state"));
    paths.ensure().expect("paths");
    let app = contextwake::app::Application {
        store: Store::open(paths.state_db()).expect("store"),
        paths,
        config: contextwake::config::AppConfig::default(),
        git: GitClient::new(Duration::from_secs(5)),
        agents: AgentRegistry::discover(),
    };
    let directory = root.path().join("workspace");
    fs::create_dir_all(&directory).expect("workspace");
    let workspace = app.status(&directory).expect("status").workspace;
    let checkpoint = app
        .checkpoints()
        .create(&workspace, None, None, objective("Keep status usable"))
        .expect("checkpoint");
    fs::remove_file(
        app.store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("path"),
    )
    .expect("delete checkpoint file");

    let status = app.status(&directory).expect("status survives");
    assert_eq!(status.continuity_guardian.latest_checkpoint_id, None);
}

#[test]
fn switch_checkpoints_do_not_inherit_stale_validation_results() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let personal = profile(&local.paths, "personal");
    let work = profile(&local.paths, "work");
    local.store.insert_profile(&personal).expect("personal");
    local.store.insert_profile(&work).expect("work");
    local
        .store
        .set_active_profile(personal.id)
        .expect("activate");
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    let validated = local
        .checkpoints
        .create(
            &workspace,
            Some(&personal),
            None,
            CheckpointInput {
                objective: "Ship CSV export".into(),
                validation_results: vec![ValidationResult {
                    command: vec!["cargo".into(), "test".into()],
                    status: ValidationStatus::Passed,
                    observed_at: Utc::now() - chrono::Duration::days(3),
                    summary: "exit status 0".into(),
                }],
                ..CheckpointInput::default()
            },
        )
        .expect("validated checkpoint");
    assert_eq!(validated.validation_results.len(), 1);

    let outcome = switch_profile(
        &local.store,
        &local.checkpoints,
        &local.handoffs,
        "work",
        Some(&workspace),
        true,
        None,
    )
    .expect("switch");
    let handoff = outcome.handoff.expect("handoff");
    let switched = local
        .checkpoints
        .read(&handoff.checkpoint_id.to_string())
        .expect("switch checkpoint");
    assert!(switched.validation_results.is_empty());
    let context = local
        .handoffs
        .context(&handoff.id.to_string())
        .expect("context");
    assert!(context.contains("No validation results were recorded with this checkpoint."));
    assert!(!context.contains("Passed"));
}

#[test]
fn provenance_distinguishes_local_and_imported_handoffs() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    let checkpoint = local
        .checkpoints
        .create(&workspace, None, None, objective("Continue"))
        .expect("checkpoint");
    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");
    let id = record.id.to_string();
    assert_eq!(
        local.handoffs.provenance(&id).expect("provenance"),
        HandoffProvenance::Local
    );
    let verified = local.handoffs.verify(&id).expect("verified");
    assert_eq!(verified.provenance, HandoffProvenance::Local);
    assert!(
        verified
            .untrusted_data_framing()
            .contains("generated on this machine")
    );

    let exported = root.path().join("exported");
    local
        .handoffs
        .export(&id, &exported, false)
        .expect("export");
    let other = state(&root.path().join("other"));
    let imported = other.handoffs.import_directory(&exported).expect("import");
    assert_eq!(
        HandoffProvenance::of(&imported),
        HandoffProvenance::Imported
    );
    let verified = other.handoffs.verify(&id).expect("verified import");
    assert_eq!(verified.provenance, HandoffProvenance::Imported);
    // The imported manifest still names the sender's checkpoint; provenance
    // comes from the local record instead.
    assert_eq!(verified.manifest.checkpoint_id, checkpoint.id);
    let framing = verified.untrusted_data_framing();
    assert!(framing.contains("imported from an external package"));
    assert!(framing.contains(&id));
}

fn read_manifest(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("manifest.json")).expect("manifest"))
        .expect("manifest JSON")
}

fn edit_manifest(directory: &Path, edit: impl FnOnce(&mut Value)) {
    let mut manifest = read_manifest(directory);
    edit(&mut manifest);
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
    )
    .expect("write manifest");
}

/// Writes a payload and records it in the manifest with a matching digest, so
/// the import check under test is reached instead of the integrity check.
fn put_payload(directory: &Path, manifest: &mut Value, name: &str, bytes: &[u8]) {
    fs::write(directory.join(name), bytes).expect("payload");
    let digest = sha256_bytes(bytes);
    let files = manifest["content_files"]
        .as_array_mut()
        .expect("content files");
    if let Some(existing) = files.iter_mut().find(|entry| entry["path"] == name) {
        existing["sha256"] = json!(digest);
    } else {
        files.push(json!({"path": name, "sha256": digest, "media_type": "text/markdown"}));
    }
}

fn copy_package(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("package directory");
    for entry in fs::read_dir(source).expect("package") {
        let entry = entry.expect("entry");
        fs::copy(entry.path(), destination.join(entry.file_name())).expect("copy payload");
    }
}

fn valid_exported_package(root: &Path) -> PathBuf {
    let local = state(&root.join("source-state"));
    let workspace = registered_workspace(&local.store, &root.join("source-workspace"), None);
    let checkpoint = local
        .checkpoints
        .create(
            &workspace,
            None,
            None,
            CheckpointInput {
                objective: "Add CSV export".into(),
                pending_tasks: vec!["Quoting".into()],
                ..CheckpointInput::default()
            },
        )
        .expect("checkpoint");
    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");
    let exported = root.join("valid-package");
    local
        .handoffs
        .export(&record.id.to_string(), &exported, false)
        .expect("export");
    exported
}

#[test]
fn import_rejects_each_malformed_package_property() {
    type Mutation = fn(&Path);
    let cases: [(&str, Mutation, &str); 28] = [
        (
            "nested payload path",
            |dir| {
                fs::create_dir(dir.join("nested")).expect("nested");
                fs::rename(
                    dir.join("context.md"),
                    dir.join("nested").join("context.md"),
                )
                .expect("move");
                edit_manifest(dir, |m| {
                    m["content_files"][0]["path"] = json!("nested/context.md");
                });
            },
            "must use a top-level filename",
        ),
        (
            "parent traversal",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"][0]["path"] = json!("../context.md");
                });
            },
            "invalid or unsafe path",
        ),
        (
            "absolute payload path",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"][0]["path"] = json!("/etc/passwd");
                });
            },
            "invalid or unsafe path",
        ),
        (
            "duplicate entry",
            |dir| {
                edit_manifest(dir, |m| {
                    let first = m["content_files"][0].clone();
                    m["content_files"]
                        .as_array_mut()
                        .expect("files")
                        .push(first);
                });
            },
            "duplicate handoff content path context.md",
        ),
        (
            "unsupported media type",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"][0]["media_type"] = json!("text/html");
                });
            },
            "unsupported handoff media type text/html",
        ),
        (
            "oversized payload",
            |dir| {
                edit_manifest(dir, |m| {
                    put_payload(dir, m, "big.md", &vec![b'a'; 1024 * 1024 + 1]);
                });
            },
            "exceeds the 1048576 byte limit",
        ),
        (
            "package over 4 MiB",
            |dir| {
                edit_manifest(dir, |m| {
                    for part in 0..5 {
                        put_payload(dir, m, &format!("part-{part}.md"), &vec![b'a'; 900_000]);
                    }
                });
            },
            "exceeds the 4 MiB import limit",
        ),
        (
            "secret in manifest",
            |dir| {
                edit_manifest(dir, |m| {
                    m["objective"] = json!("Use sk-abcdefghijklmnopqrstuvwxyz12 to deploy");
                });
            },
            "manifest contains possible secret material",
        ),
        (
            "secret in payload",
            |dir| {
                edit_manifest(dir, |m| {
                    put_payload(
                        dir,
                        m,
                        "context.md",
                        b"# Handoff\n\ntoken=supersecretvalue123\n",
                    );
                });
            },
            "context.md contains possible secret material",
        ),
        (
            "missing untrusted-notes flag",
            |dir| edit_manifest(dir, |m| m["security_flags"] = json!([])),
            "does not declare commands as untrusted notes",
        ),
        (
            "missing context entry",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"].as_array_mut().expect("files").remove(0);
                });
            },
            "must include context.md",
        ),
        (
            "missing payload file",
            |dir| fs::remove_file(dir.join("validation.json")).expect("remove"),
            "validation.json",
        ),
        (
            "unsupported schema version",
            |dir| edit_manifest(dir, |m| m["schema_version"] = json!("9.0.0")),
            "unsupported handoff schema 9.0.0",
        ),
        (
            "too many content files",
            |dir| {
                edit_manifest(dir, |m| {
                    let files = m["content_files"].as_array_mut().expect("files");
                    for index in 0..14 {
                        files.push(json!({
                            "path": format!("extra-{index}.md"),
                            "sha256": "0".repeat(64),
                            "media_type": "text/markdown",
                        }));
                    }
                });
            },
            "more than 16 content files",
        ),
        (
            "terminal control in manifest",
            |dir| {
                edit_manifest(dir, |m| {
                    m["workspace"]["display_name"] = json!("repo\u{1b}[31m");
                });
            },
            "handoff manifest contains terminal control data",
        ),
        (
            "terminal control in payload",
            |dir| {
                edit_manifest(dir, |m| {
                    put_payload(dir, m, "context.md", b"# Handoff\n\x1b]0;owned\x07\n");
                });
            },
            "context.md contains terminal control data",
        ),
        (
            "alternate data stream name",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"]
                        .as_array_mut()
                        .expect("files")
                        .push(json!({
                            "path": "context.md:hidden",
                            "sha256": "0".repeat(64),
                            "media_type": "text/markdown",
                        }));
                });
            },
            "not a portable filename",
        ),
        (
            "reserved device name",
            |dir| {
                edit_manifest(dir, |m| {
                    m["content_files"]
                        .as_array_mut()
                        .expect("files")
                        .push(json!({
                            "path": "NUL.json",
                            "sha256": "0".repeat(64),
                            "media_type": "application/json",
                        }));
                });
            },
            "not a portable filename",
        ),
        (
            "non-UTF-8 payload",
            |dir| {
                edit_manifest(dir, |m| {
                    put_payload(dir, m, "notes.md", &[0xff, 0xfe, 0xfd]);
                });
            },
            "notes.md is not valid UTF-8",
        ),
        (
            "tampered payload",
            |dir| fs::write(dir.join("context.md"), "tampered").expect("tamper"),
            "integrity check failed for context.md",
        ),
        (
            "invalid manifest JSON",
            |dir| fs::write(dir.join("manifest.json"), "{").expect("manifest"),
            "invalid handoff manifest",
        ),
        (
            "unknown manifest field",
            |dir| edit_manifest(dir, |m| m["unexpected"] = json!(true)),
            "invalid handoff manifest",
        ),
        (
            "AWHF 1.2 without source",
            |dir| {
                edit_manifest(dir, |m| {
                    m.as_object_mut().expect("object").remove("source");
                });
            },
            "require structured source agent metadata",
        ),
        (
            "AWHF 1.2 without continuity mode",
            |dir| {
                edit_manifest(dir, |m| {
                    m.as_object_mut().expect("object").remove("continuity_mode");
                });
            },
            "continuity_mode=portable_handoff",
        ),
        (
            "option-like agent id",
            |dir| edit_manifest(dir, |m| m["source"]["agent_id"] = json!("--exec")),
            "cannot begin with '-'",
        ),
        (
            "oversized manifest",
            |dir| {
                edit_manifest(dir, |m| {
                    m["extensions"] = json!({"x-padding": "a".repeat(1024 * 1024)});
                });
            },
            "manifest.json exceeds the 1048576 byte limit",
        ),
        (
            "manifest replaced by a directory",
            |dir| {
                fs::remove_file(dir.join("manifest.json")).expect("remove");
                fs::create_dir(dir.join("manifest.json")).expect("directory");
            },
            "must be a real file",
        ),
        (
            "payload replaced by a directory",
            |dir| {
                fs::remove_file(dir.join("validation.json")).expect("remove");
                fs::create_dir(dir.join("validation.json")).expect("directory");
            },
            "must be a real file",
        ),
    ];

    let root = tempfile::tempdir().expect("root");
    let valid = valid_exported_package(root.path());
    import_fresh(root.path(), &valid).expect("the unmodified package imports");
    for (index, (name, mutate, expected)) in cases.iter().enumerate() {
        let package = root.path().join(format!("case-{index}"));
        copy_package(&valid, &package);
        mutate(&package);
        let error =
            import_fresh(root.path(), &package).expect_err(&format!("{name} must be rejected"));
        assert!(
            error.to_string().contains(expected),
            "{name}: expected {expected:?}, got: {error}"
        );
    }
}

#[test]
fn import_refuses_linked_directories_and_duplicate_imports() {
    let root = tempfile::tempdir().expect("root");
    let valid = valid_exported_package(root.path());
    let linked = root.path().join("linked-package");
    link_directory(&valid, &linked);
    let error = import_fresh(root.path(), &linked).expect_err("linked directory");
    assert!(
        error.to_string().contains("must be a real directory"),
        "{error}"
    );

    let target = state(&root.path().join("target"));
    target
        .handoffs
        .import_directory(&valid)
        .expect("first import");
    let error = target
        .handoffs
        .import_directory(&valid)
        .expect_err("second import");
    assert!(error.to_string().contains("is already present"), "{error}");
}

/// Creates a directory link without elevated rights: a junction on Windows,
/// a symlink elsewhere. Both are reparse points/symlinks to `symlink_metadata`.
#[cfg(windows)]
fn link_directory(target: &Path, link: &Path) {
    let parent = link.parent().expect("link parent");
    let cmd = std::env::var("SystemRoot").map_or_else(
        |_| PathBuf::from(r"C:\Windows\System32\cmd.exe"),
        |root| PathBuf::from(root).join("System32").join("cmd.exe"),
    );
    let status = Command::new(cmd)
        .current_dir(parent)
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link.file_name().expect("link name"))
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("mklink");
    assert!(status.success(), "could not create a junction");
}

#[cfg(unix)]
fn link_directory(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("symlink");
}

#[test]
fn created_checkpoint_records_are_never_marked_redacted_without_cause() {
    let root = tempfile::tempdir().expect("root");
    let local = state(&root.path().join("state"));
    let workspace = registered_workspace(&local.store, &root.path().join("workspace"), None);
    let checkpoint = local
        .checkpoints
        .create(&workspace, None, None, objective("Plain objective"))
        .expect("checkpoint");
    assert_eq!(checkpoint.redaction_status, RedactionStatus::Clean);
    let record = local
        .handoffs
        .create(&checkpoint, &workspace)
        .expect("handoff");
    let manifest = local
        .handoffs
        .manifest(&record.id.to_string())
        .expect("manifest");
    assert_eq!(
        manifest.security_flags,
        ["commands_are_untrusted_notes_only"]
    );
    assert!(manifest.redactions.is_empty());
}
