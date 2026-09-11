use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use crate::error::{AgentDeckError, IoContext, Result};
use crate::git::GitClient;
use crate::model::{Checkpoint, Profile, RedactionStatus, Session, ValidationResult, Workspace};
use crate::paths::AppPaths;
use crate::security::{SecretScanner, sanitize_terminal};
use crate::store::Store;
use crate::workspace::load_project_instructions;

#[derive(Clone, Debug, Default)]
pub struct CheckpointInput {
    pub objective: String,
    pub active_task: Option<String>,
    pub completed: Vec<String>,
    pub decisions: Vec<String>,
    pub pending_tasks: Vec<String>,
    pub known_issues: Vec<String>,
    pub user_notes: Option<String>,
    pub validation_results: Vec<ValidationResult>,
}

impl CheckpointInput {
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        Self {
            objective: checkpoint.objective.clone(),
            active_task: checkpoint.active_task.clone(),
            completed: checkpoint.completed.clone(),
            decisions: checkpoint.decisions.clone(),
            pending_tasks: checkpoint.pending_tasks.clone(),
            known_issues: checkpoint.known_issues.clone(),
            user_notes: checkpoint.user_notes.clone(),
            validation_results: checkpoint.validation_results.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CheckpointService {
    store: Store,
    paths: AppPaths,
    git: GitClient,
}

impl CheckpointService {
    pub fn new(store: Store, paths: AppPaths, git: GitClient) -> Self {
        Self { store, paths, git }
    }

    pub fn create(
        &self,
        workspace: &Workspace,
        profile: Option<&Profile>,
        session: Option<&Session>,
        input: CheckpointInput,
    ) -> Result<Checkpoint> {
        if input.objective.trim().is_empty() {
            return Err(AgentDeckError::InvalidData(
                "checkpoint objective cannot be empty".into(),
            ));
        }
        let scanner = SecretScanner::new();
        let mut redacted = false;
        let mut clean = |value: String| {
            let safe = sanitize_terminal(&value);
            redacted |= safe != value;
            let report = scanner.redact(&safe);
            redacted |= report.redacted() || report.text.contains("[REDACTED]");
            report.text.trim().to_string()
        };
        let objective = clean(input.objective);
        let active_task = input
            .active_task
            .map(&mut clean)
            .filter(|value| !value.is_empty());
        let completed = input.completed.into_iter().map(&mut clean).collect();
        let decisions = input.decisions.into_iter().map(&mut clean).collect();
        let pending_tasks = input.pending_tasks.into_iter().map(&mut clean).collect();
        let known_issues = input.known_issues.into_iter().map(&mut clean).collect();
        let user_notes = input
            .user_notes
            .map(&mut clean)
            .filter(|value| !value.is_empty());
        let validation_results = input
            .validation_results
            .into_iter()
            .map(|mut result| {
                result.command = result.command.into_iter().map(&mut clean).collect();
                result.summary = clean(result.summary);
                result
            })
            .collect();
        let project_instructions = load_project_instructions(workspace)?
            .map(&mut clean)
            .filter(|value| !value.is_empty());

        let git = self.git.snapshot(&workspace.path)?;
        let files_changed = self.git.changed_files(&workspace.path)?;
        let checkpoint = Checkpoint {
            schema_version: 2,
            id: Uuid::new_v4(),
            workspace_id: workspace.id,
            session_id: session.map(|value| value.id),
            agent_id: profile.map(|value| value.agent_id.clone()),
            model_provider_id: profile.and_then(|value| value.model_provider_id.clone()),
            model: profile.and_then(|value| value.model_preference.clone()),
            profile_id: profile.map(|value| value.id),
            objective,
            active_task,
            completed,
            decisions,
            pending_tasks,
            known_issues,
            user_notes,
            project_instructions,
            files_changed,
            git,
            validation_results,
            created_at: Utc::now(),
            redaction_status: if redacted {
                RedactionStatus::Redacted
            } else {
                RedactionStatus::Clean
            },
        };
        let path = self
            .paths
            .checkpoints_dir()
            .join(format!("{}.json", checkpoint.id));
        let content = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            AgentDeckError::InvalidData(format!("could not serialize checkpoint: {error}"))
        })?;
        atomic_write(&path, &content, false)?;
        if let Err(error) = self.store.insert_checkpoint_index(
            checkpoint.id,
            checkpoint.workspace_id,
            checkpoint.session_id,
            checkpoint.profile_id,
            checkpoint.agent_id.as_deref(),
            checkpoint.model_provider_id.as_deref(),
            checkpoint.model.as_deref(),
            &checkpoint.objective,
            &path,
            checkpoint.redaction_status,
            checkpoint.created_at,
        ) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(checkpoint)
    }

    pub fn read(&self, reference: &str) -> Result<Checkpoint> {
        let path = self.store.checkpoint_path(reference)?;
        ensure_managed_file(&path, &self.paths.checkpoints_dir())?;
        let content = std::fs::read_to_string(&path).at(&path)?;
        let checkpoint: Checkpoint = serde_json::from_str(&content).map_err(|error| {
            AgentDeckError::InvalidData(format!("invalid checkpoint {}: {error}", path.display()))
        })?;
        if !matches!(checkpoint.schema_version, 1 | 2) || checkpoint.id.to_string() != reference {
            return Err(AgentDeckError::InvalidData(format!(
                "checkpoint index/content mismatch for {reference}"
            )));
        }
        Ok(checkpoint)
    }

    pub fn list(&self) -> Result<Vec<Checkpoint>> {
        self.store
            .list_checkpoint_paths()?
            .into_iter()
            .map(|(id, _, _, _)| self.read(&id.to_string()))
            .collect()
    }

    pub fn latest_for_workspace(&self, workspace_id: Uuid) -> Result<Option<Checkpoint>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|checkpoint| checkpoint.workspace_id == workspace_id))
    }

    pub fn delete(&self, reference: &str) -> Result<()> {
        let path = self.store.checkpoint_path(reference)?;
        ensure_managed_file(&path, &self.paths.checkpoints_dir())?;
        std::fs::remove_file(&path).at(&path)?;
        self.store.delete_checkpoint_index(reference)?;
        Ok(())
    }

    pub fn export_json(
        &self,
        reference: &str,
        destination: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        let checkpoint = self.read(reference)?;
        let content = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            AgentDeckError::InvalidData(format!("could not serialize checkpoint: {error}"))
        })?;
        atomic_write(destination, &content, overwrite)?;
        Ok(destination.to_path_buf())
    }
}

pub(crate) fn atomic_write(path: &Path, content: &[u8], overwrite: bool) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AgentDeckError::UnsafePath(format!("{} has no parent directory", path.display()))
    })?;
    std::fs::create_dir_all(parent).at(parent)?;
    if path.exists() && !overwrite {
        return Err(AgentDeckError::InvalidData(format!(
            "{} already exists; pass --force to replace it",
            path.display()
        )));
    }
    if path.exists()
        && std::fs::symlink_metadata(path)
            .at(path)?
            .file_type()
            .is_symlink()
    {
        return Err(AgentDeckError::UnsafePath(format!(
            "refusing to replace symlink {}",
            path.display()
        )));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).at(parent)?;
    temporary.write_all(content).at(temporary.path())?;
    temporary.flush().at(temporary.path())?;
    temporary.as_file().sync_all().at(temporary.path())?;
    temporary
        .persist(path)
        .map_err(|error| AgentDeckError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

fn ensure_managed_file(path: &Path, base: &Path) -> Result<()> {
    let canonical_base = base.canonicalize().at(base)?;
    let canonical_path = path.canonicalize().at(path)?;
    if !canonical_path.starts_with(&canonical_base)
        || std::fs::symlink_metadata(path)
            .at(path)?
            .file_type()
            .is_symlink()
    {
        return Err(AgentDeckError::UnsafePath(format!(
            "{} is outside managed checkpoint storage or is a symlink",
            sanitize_terminal(&path.display().to_string())
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrustState;

    #[test]
    fn atomic_write_refuses_accidental_overwrite() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("checkpoint.json");
        atomic_write(&path, b"first", false).expect("first write");
        assert!(atomic_write(&path, b"second", false).is_err());
        assert_eq!(std::fs::read(path).expect("read"), b"first");
    }

    #[test]
    fn atomic_write_replaces_existing_file_when_explicit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("checkpoint.json");
        atomic_write(&path, b"first", false).expect("first write");
        atomic_write(&path, b"second", true).expect("explicit replacement");
        assert_eq!(std::fs::read(path).expect("read"), b"second");
    }

    #[test]
    fn checkpoint_redacts_secrets_before_persistence() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths::from_root(root.path());
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        let git = GitClient::with_executable(
            "definitely-missing-git",
            std::time::Duration::from_millis(10),
        );
        let service = CheckpointService::new(store.clone(), paths, git);
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: root.path().to_path_buf(),
            display_name: "test".into(),
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
        store.upsert_workspace(&workspace).expect("workspace");
        let result = service.create(
            &workspace,
            None,
            None,
            CheckpointInput {
                objective: "Do work with sk-abcdefghijklmnopqrstuvwxyz".into(),
                ..CheckpointInput::default()
            },
        );
        // Missing Git is actionable rather than silently swallowed.
        assert!(matches!(result, Err(AgentDeckError::Git(_))));
    }
}
