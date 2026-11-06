use crate::checkpoint::{CheckpointInput, CheckpointService};
use crate::error::Result;
use crate::handoff::HandoffService;
use crate::model::{Checkpoint, HandoffRecord, Profile, Workspace};
use crate::store::Store;

#[derive(Clone, Debug)]
pub struct SwitchOutcome {
    pub previous: Option<Profile>,
    pub active: Profile,
    pub handoff: Option<HandoffRecord>,
    pub message: String,
}

pub fn switch_profile(
    store: &Store,
    checkpoints: &CheckpointService,
    handoffs: &HandoffService,
    target_reference: &str,
    workspace: Option<&Workspace>,
    create_handoff: bool,
    objective: Option<String>,
) -> Result<SwitchOutcome> {
    let previous = store.active_profile()?;
    let target = store.profile(target_reference)?;
    if previous
        .as_ref()
        .is_some_and(|profile| profile.id == target.id)
    {
        return Ok(SwitchOutcome {
            previous,
            active: target,
            handoff: None,
            message: "Profile was already active; no session transfer was attempted".into(),
        });
    }

    let created = if create_handoff {
        let workspace = workspace.ok_or_else(|| {
            crate::error::ContextWakeError::InvalidData(
                "a workspace is required to create a switch handoff".into(),
            )
        })?;
        let previous_checkpoint = checkpoints.latest_for_workspace(workspace.id)?;
        let mut input = previous_checkpoint
            .as_ref()
            .map_or_else(CheckpointInput::default, CheckpointInput::from_checkpoint);
        if let Some(objective) = objective.filter(|value| !value.trim().is_empty()) {
            input.objective = objective;
        } else if input.objective.trim().is_empty() {
            input.objective = format!("Continue work in {}", workspace.display_name);
        }
        if input.active_task.is_none() {
            input.active_task = Some(format!(
                "Continue from profile {} using an explicit workspace handoff",
                previous
                    .as_ref()
                    .map_or("unknown", |profile| profile.display_name.as_str())
            ));
        }
        input
            .pending_tasks
            .retain(|task| !task.starts_with("Review this handoff after activating profile "));
        let review_task = format!(
            "Review this handoff after activating profile {}",
            target.display_name
        );
        if !input.pending_tasks.contains(&review_task) {
            input.pending_tasks.push(review_task);
        }
        let checkpoint = checkpoints.create(workspace, previous.as_ref(), None, input)?;
        match handoffs.create_for_destination(&checkpoint, workspace, Some(&target)) {
            Ok(handoff) => Some((checkpoint, handoff)),
            Err(error) => {
                // A switch that did not happen must not leave a checkpoint behind:
                // it would become the workspace's latest checkpoint and carry a
                // review task for an activation that never took place.
                discard_checkpoint(checkpoints, &checkpoint);
                return Err(error);
            }
        }
    } else {
        None
    };

    // Activation is the final mutation. A failed checkpoint/handoff therefore leaves
    // the previously valid active profile untouched, and a failed activation
    // removes the artifacts this switch created.
    if let Err(error) = store.set_active_profile(target.id) {
        if let Some((checkpoint, handoff)) = &created {
            discard_handoff(store, handoff);
            discard_checkpoint(checkpoints, checkpoint);
        }
        return Err(error);
    }
    let handoff = created.map(|(_, handoff)| handoff);
    let message = if handoff.is_some() {
        "Profile switched. Continuity was preserved with a workspace handoff; no native session resume was claimed."
    } else {
        "Profile switched explicitly. No provider session was resumed or transferred."
    };
    Ok(SwitchOutcome {
        previous,
        active: target,
        handoff,
        message: message.into(),
    })
}

/// Best-effort removal of a checkpoint created by a switch that failed; the
/// caller reports the original error.
fn discard_checkpoint(checkpoints: &CheckpointService, checkpoint: &Checkpoint) {
    let _ = checkpoints.delete(&checkpoint.id.to_string());
}

/// Best-effort removal of a handoff created by a switch that failed. The
/// directory was created by this switch inside managed storage.
fn discard_handoff(store: &Store, handoff: &HandoffRecord) {
    let _ = std::fs::remove_dir_all(&handoff.storage_path);
    let _ = store.delete_handoff_index(handoff.id);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::git::GitClient;
    use crate::model::{AuthState, TrustState};
    use crate::paths::AppPaths;

    struct Fixture {
        root: tempfile::TempDir,
        paths: AppPaths,
        store: Store,
        personal: Profile,
        workspace: Workspace,
        checkpoints: CheckpointService,
        source: Checkpoint,
    }

    fn profile(paths: &AppPaths, name: &str) -> Profile {
        let now = Utc::now();
        let id = Uuid::new_v4();
        Profile {
            id,
            name: name.into(),
            display_name: name.into(),
            agent_id: "codex".into(),
            model_provider_id: None,
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

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path().join("state"));
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
            GitClient::new(Duration::from_secs(10)),
        );
        let source = checkpoints
            .create(
                &workspace,
                Some(&personal),
                None,
                CheckpointInput {
                    objective: "Implement checked division".into(),
                    pending_tasks: vec!["Division tests".into()],
                    ..CheckpointInput::default()
                },
            )
            .expect("source checkpoint");
        Fixture {
            root,
            paths,
            store,
            personal,
            workspace,
            checkpoints,
            source,
        }
    }

    fn assert_only_source_checkpoint_remains(fixture: &Fixture) {
        let indexed = fixture.store.list_checkpoint_paths().expect("index");
        assert_eq!(
            indexed.iter().map(|row| row.0).collect::<Vec<_>>(),
            vec![fixture.source.id],
            "the failed switch's checkpoint must be removed from the index"
        );
        let files = std::fs::read_dir(fixture.paths.checkpoints_dir())
            .expect("checkpoint directory")
            .count();
        assert_eq!(
            files, 1,
            "the failed switch's checkpoint file must be removed"
        );
        assert_eq!(
            fixture
                .checkpoints
                .latest_for_workspace(fixture.workspace.id)
                .expect("latest")
                .map(|checkpoint| checkpoint.id),
            Some(fixture.source.id)
        );
        assert_eq!(
            fixture
                .store
                .active_profile()
                .expect("active")
                .expect("profile")
                .id,
            fixture.personal.id
        );
    }

    #[test]
    fn failed_handoff_creation_removes_the_switch_checkpoint() {
        let fixture = fixture();
        // Handoff storage that does not exist makes handoff creation fail after
        // the switch checkpoint has been written.
        let unavailable_handoffs = HandoffService::new(
            fixture.store.clone(),
            AppPaths::from_root(fixture.root.path().join("missing-state")),
        );
        let result = switch_profile(
            &fixture.store,
            &fixture.checkpoints,
            &unavailable_handoffs,
            "work",
            Some(&fixture.workspace),
            true,
            None,
        );
        assert!(result.is_err());
        assert_only_source_checkpoint_remains(&fixture);
    }

    #[test]
    fn failed_activation_removes_the_switch_checkpoint_and_handoff() {
        let fixture = fixture();
        let handoffs = HandoffService::new(fixture.store.clone(), fixture.paths.clone());
        // The final activation fails after the checkpoint and handoff exist.
        rusqlite::Connection::open(fixture.store.path())
            .expect("database")
            .execute_batch(
                "CREATE TRIGGER reject_profile_switch BEFORE UPDATE ON settings
                 WHEN NEW.key = 'active_profile_id'
                 BEGIN SELECT RAISE(ABORT, 'injected activation failure'); END;",
            )
            .expect("trigger");
        let result = switch_profile(
            &fixture.store,
            &fixture.checkpoints,
            &handoffs,
            "work",
            Some(&fixture.workspace),
            true,
            Some("Continue division".into()),
        );
        assert!(result.is_err());
        assert!(fixture.store.list_handoffs().expect("handoffs").is_empty());
        assert_eq!(
            std::fs::read_dir(fixture.paths.handoffs_dir())
                .expect("handoff directory")
                .count(),
            0,
            "the failed switch's handoff directory must be removed"
        );
        assert_only_source_checkpoint_remains(&fixture);
    }
}
