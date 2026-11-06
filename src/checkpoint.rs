use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::git::GitClient;
use crate::model::{
    Checkpoint, ConversationExcerpt, Profile, RedactionStatus, Session, ValidationResult, Workspace,
};
use crate::paths::{AppPaths, simplify_path};
use crate::security::{SecretScanner, sanitize_terminal, sanitize_untrusted_label};
use crate::store::Store;
use crate::workspace::load_project_instructions;

/// Schema version of the checkpoints this build writes. Version 3 added the
/// optional `conversation` excerpt; readers accept versions 1 to 3.
pub const CHECKPOINT_SCHEMA_VERSION: u32 = 3;
/// `maxLength` of free-text fields in `schemas/checkpoint-v3.schema.json`.
const TEXT_FIELD_MAX_CHARS: usize = 65_536;
/// `maxLength` of validation command items in the checkpoint schema.
const COMMAND_ITEM_MAX_CHARS: usize = 4_096;
/// `maxLength` of the excerpt's session id and title in the checkpoint schema.
const EXCERPT_SESSION_ID_MAX_CHARS: usize = 256;
const EXCERPT_TITLE_MAX_CHARS: usize = 200;

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
    /// Seeds a new checkpoint with an earlier checkpoint's explicit project
    /// state (objective, tasks, decisions, issues and notes).
    ///
    /// Validation results are deliberately not carried over: they describe the
    /// tree at the moment they were observed, while the new checkpoint captures
    /// a new Git state, so a copied pass would read as current when it is not.
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        Self {
            objective: checkpoint.objective.clone(),
            active_task: checkpoint.active_task.clone(),
            completed: checkpoint.completed.clone(),
            decisions: checkpoint.decisions.clone(),
            pending_tasks: checkpoint.pending_tasks.clone(),
            known_issues: checkpoint.known_issues.clone(),
            user_notes: checkpoint.user_notes.clone(),
            validation_results: Vec::new(),
        }
    }
}

/// A checkpoint index entry whose JSON file could not be used: it is missing,
/// unreadable, corrupt, outside managed storage, a symlink, or written with an
/// unsupported schema version. Listing skips such entries instead of failing,
/// and reports them here so interfaces can show them with a recovery command.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UnreadableCheckpoint {
    pub id: Uuid,
    /// Objective recorded in the index when the checkpoint was created.
    pub objective: String,
    pub created_at: DateTime<Utc>,
    pub storage_path: PathBuf,
    pub reason: String,
}

impl UnreadableCheckpoint {
    fn new(
        id: Uuid,
        objective: &str,
        storage_path: PathBuf,
        created_at: DateTime<Utc>,
        reason: &str,
    ) -> Self {
        Self {
            id,
            objective: sanitize_terminal(objective),
            created_at,
            storage_path,
            reason: sanitize_terminal(reason),
        }
    }

    fn from_error(
        id: Uuid,
        objective: &str,
        storage_path: PathBuf,
        created_at: DateTime<Utc>,
        error: &ContextWakeError,
    ) -> Self {
        let reason = if matches!(error, ContextWakeError::CheckpointNotFound(_)) {
            "checkpoint file is missing".to_string()
        } else {
            error.to_string()
        };
        Self::new(id, objective, storage_path, created_at, &reason)
    }

    /// The command that removes this stale index entry.
    pub fn recovery_command(&self) -> String {
        format!("ctx checkpoint delete {} --yes", self.id)
    }
}

/// Every indexed checkpoint, split into readable checkpoints (newest first)
/// and entries that could not be read.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct CheckpointListing {
    pub checkpoints: Vec<Checkpoint>,
    pub unreadable: Vec<UnreadableCheckpoint>,
}

/// The newest readable checkpoint of one workspace, plus any newer index
/// entries of that workspace that could not be read.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct LatestCheckpoint {
    pub checkpoint: Option<Checkpoint>,
    pub unreadable: Vec<UnreadableCheckpoint>,
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
        self.create_with_conversation(workspace, profile, session, input, None)
    }

    /// Like [`Self::create`], and also stores `conversation`, the recent
    /// conversation of the source agent in this workspace. The excerpt comes
    /// from a provider transcript, so it is sanitized, redacted and bounded
    /// again here like every other free-text field.
    pub fn create_with_conversation(
        &self,
        workspace: &Workspace,
        profile: Option<&Profile>,
        session: Option<&Session>,
        input: CheckpointInput,
        conversation: Option<ConversationExcerpt>,
    ) -> Result<Checkpoint> {
        if input.objective.trim().is_empty() {
            return Err(ContextWakeError::InvalidData(
                "checkpoint objective cannot be empty".into(),
            ));
        }
        let scanner = SecretScanner::new();
        let mut redacted = false;
        let mut clean = |value: String, max_chars: usize| {
            let safe = sanitize_terminal(&value);
            redacted |= safe != value;
            let report = scanner.redact(&safe);
            redacted |= report.redacted() || report.text.contains("[REDACTED]");
            // Bound only after redaction, so a cut can never expose part of a
            // secret; `fit_redacted` re-scans whatever it cuts.
            let (text, cut_redacted) = fit_redacted(&scanner, report.text.trim(), max_chars);
            redacted |= cut_redacted;
            text
        };
        let objective = clean(input.objective, TEXT_FIELD_MAX_CHARS);
        if objective.is_empty() {
            return Err(ContextWakeError::InvalidData(
                "checkpoint objective cannot be empty after terminal control data is removed"
                    .into(),
            ));
        }
        let active_task = input
            .active_task
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .filter(|value| !value.is_empty());
        let completed = input
            .completed
            .into_iter()
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .collect();
        let decisions = input
            .decisions
            .into_iter()
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .collect();
        let pending_tasks = input
            .pending_tasks
            .into_iter()
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .collect();
        let known_issues = input
            .known_issues
            .into_iter()
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .collect();
        let user_notes = input
            .user_notes
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .filter(|value| !value.is_empty());
        let validation_results = input
            .validation_results
            .into_iter()
            .map(|mut result| {
                result.command = result
                    .command
                    .into_iter()
                    .map(|value| clean(value, COMMAND_ITEM_MAX_CHARS))
                    .collect();
                result.summary = clean(result.summary, TEXT_FIELD_MAX_CHARS);
                result
            })
            .collect();
        let project_instructions = load_project_instructions(workspace)?
            .map(|value| clean(value, TEXT_FIELD_MAX_CHARS))
            .filter(|value| !value.is_empty());

        // The commit subject is repository free text and is redacted like any
        // other field before it is written; the repository root is stored in
        // its conventional form. HEAD and the branch stay raw because the
        // guardian compares them with later snapshots.
        let git = self.git.snapshot(&workspace.path)?.map(|mut snapshot| {
            snapshot.last_commit = snapshot
                .last_commit
                .take()
                .map(|subject| clean(subject, TEXT_FIELD_MAX_CHARS))
                .filter(|subject| !subject.is_empty());
            snapshot.repository_root = snapshot.repository_root.as_deref().map(simplify_path);
            snapshot
        });
        let files_changed = self.git.changed_files(&workspace.path)?;
        let conversation = conversation.and_then(|excerpt| clean_conversation(excerpt, &mut clean));
        let checkpoint = Checkpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
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
            conversation,
        };
        let path = self
            .paths
            .checkpoints_dir()
            .join(format!("{}.json", checkpoint.id));
        let content = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            ContextWakeError::InvalidData(format!("could not serialize checkpoint: {error}"))
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

    /// Reads one checkpoint. An index entry whose file is missing yields
    /// [`ContextWakeError::CheckpointNotFound`] with the recovery command.
    pub fn read(&self, reference: &str) -> Result<Checkpoint> {
        let path = self.store.checkpoint_path(reference)?;
        self.read_at(reference, &path)
    }

    fn read_at(&self, reference: &str, path: &Path) -> Result<Checkpoint> {
        if let Err(error) = std::fs::symlink_metadata(path) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(ContextWakeError::CheckpointNotFound(format!(
                    "{reference} (its file {} is missing; remove the stale index entry with `ctx checkpoint delete {reference} --yes`)",
                    sanitize_terminal(&path.display().to_string())
                )));
            }
            return Err(ContextWakeError::Io {
                path: path.to_path_buf(),
                source: error,
            });
        }
        ensure_managed_file(path, &self.paths.checkpoints_dir())?;
        let content = std::fs::read_to_string(path).at(path)?;
        let checkpoint: Checkpoint = serde_json::from_str(&content).map_err(|error| {
            ContextWakeError::InvalidData(format!("invalid checkpoint {}: {error}", path.display()))
        })?;
        if !(1..=CHECKPOINT_SCHEMA_VERSION).contains(&checkpoint.schema_version) {
            return Err(ContextWakeError::InvalidData(format!(
                "checkpoint {reference} uses unsupported schema version {}; it may have been written by a newer ContextWake",
                checkpoint.schema_version
            )));
        }
        if checkpoint.id.to_string() != reference {
            return Err(ContextWakeError::InvalidData(format!(
                "checkpoint index/content mismatch for {reference}"
            )));
        }
        Ok(checkpoint)
    }

    /// Readable checkpoints, newest first. Entries that cannot be read are
    /// skipped; use [`Self::list_with_unreadable`] to report them.
    pub fn list(&self) -> Result<Vec<Checkpoint>> {
        Ok(self.list_with_unreadable()?.checkpoints)
    }

    /// Every indexed checkpoint: one missing or corrupt file never hides the
    /// others, and is reported in [`CheckpointListing::unreadable`].
    pub fn list_with_unreadable(&self) -> Result<CheckpointListing> {
        let mut listing = CheckpointListing::default();
        for (id, objective, path, created_at) in self.store.list_checkpoint_paths()? {
            match self.read_at(&id.to_string(), &path) {
                Ok(checkpoint) => listing.checkpoints.push(checkpoint),
                Err(error) => listing.unreadable.push(UnreadableCheckpoint::from_error(
                    id, &objective, path, created_at, &error,
                )),
            }
        }
        Ok(listing)
    }

    /// The newest readable checkpoint of a workspace, skipping unreadable
    /// entries (see [`Self::latest_for_workspace_with_unreadable`]).
    pub fn latest_for_workspace(&self, workspace_id: Uuid) -> Result<Option<Checkpoint>> {
        Ok(self
            .latest_for_workspace_with_unreadable(workspace_id)?
            .checkpoint)
    }

    /// Looks up the workspace's checkpoints through the index, newest first,
    /// and normally reads a single file. Newer entries of the same workspace
    /// that cannot be read are skipped and reported.
    pub fn latest_for_workspace_with_unreadable(
        &self,
        workspace_id: Uuid,
    ) -> Result<LatestCheckpoint> {
        let mut latest = LatestCheckpoint::default();
        for (id, objective, path, created_at) in self
            .store
            .list_checkpoint_paths_for_workspace(workspace_id)?
        {
            match self.read_at(&id.to_string(), &path) {
                Ok(checkpoint) if checkpoint.workspace_id == workspace_id => {
                    latest.checkpoint = Some(checkpoint);
                    break;
                }
                Ok(_) => latest.unreadable.push(UnreadableCheckpoint::new(
                    id,
                    &objective,
                    path,
                    created_at,
                    "checkpoint file belongs to a different workspace than its index entry",
                )),
                Err(error) => latest.unreadable.push(UnreadableCheckpoint::from_error(
                    id, &objective, path, created_at, &error,
                )),
            }
        }
        Ok(latest)
    }

    /// Deletes a checkpoint file and its index entry. When the file is already
    /// gone (deleted by hand, quarantined, or an earlier interrupted delete),
    /// the stale index entry is still removed. Files outside managed storage
    /// and symlinks are never deleted: for such an entry (which
    /// [`Self::list_with_unreadable`] reports as unreadable) only the index
    /// row is removed, so [`UnreadableCheckpoint::recovery_command`] works for
    /// every unreadable reason.
    pub fn delete(&self, reference: &str) -> Result<()> {
        let path = self.store.checkpoint_path(reference)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                match ensure_managed_file(&path, &self.paths.checkpoints_dir()) {
                    Ok(()) => {}
                    Err(ContextWakeError::UnsafePath(_)) => {
                        self.store.delete_checkpoint_index(reference)?;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(ContextWakeError::Io {
                        path,
                        source: error,
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ContextWakeError::Io {
                    path,
                    source: error,
                });
            }
        }
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
            ContextWakeError::InvalidData(format!("could not serialize checkpoint: {error}"))
        })?;
        atomic_write(destination, &content, overwrite)?;
        Ok(destination.to_path_buf())
    }
}

/// Re-sanitizes a provider-supplied conversation excerpt with the checkpoint's
/// field cleaner and enforces the excerpt limits. An excerpt without a usable
/// agent or session id, or without any visible text, is dropped.
fn clean_conversation(
    excerpt: ConversationExcerpt,
    clean: &mut impl FnMut(String, usize) -> String,
) -> Option<ConversationExcerpt> {
    let agent_id = crate::security::validate_external_reference(&excerpt.agent_id).ok()?;
    let provider_session_id =
        sanitize_untrusted_label(&excerpt.provider_session_id, EXCERPT_SESSION_ID_MAX_CHARS)?;
    let mut truncated = excerpt.truncated;
    let mut user_requests = excerpt
        .user_requests
        .into_iter()
        .map(|request| {
            let cleaned = clean(request, ConversationExcerpt::MAX_REQUEST_CHARS);
            truncated |= cleaned.contains("[... truncated by ContextWake:");
            cleaned
        })
        .filter(|request| !request.is_empty())
        .collect::<Vec<_>>();
    let older = user_requests
        .len()
        .saturating_sub(ConversationExcerpt::MAX_USER_REQUESTS);
    if older > 0 {
        truncated = true;
        user_requests.drain(..older);
    }
    let last_agent_message = excerpt
        .last_agent_message
        .map(|message| clean(message, ConversationExcerpt::MAX_AGENT_MESSAGE_CHARS))
        .filter(|message| !message.is_empty());
    if user_requests.is_empty() && last_agent_message.is_none() {
        return None;
    }
    Some(ConversationExcerpt {
        agent_id,
        provider_session_id,
        session_title: excerpt
            .session_title
            .as_deref()
            .and_then(|title| sanitize_untrusted_label(title, EXCERPT_TITLE_MAX_CHARS)),
        captured_at: excerpt.captured_at,
        last_activity_at: excerpt.last_activity_at,
        user_requests,
        last_agent_message,
        truncated,
    })
}

/// Cuts `text` to at most `max_bytes` bytes on a character boundary and, when
/// anything was removed, appends an explicit marker saying how much. Callers
/// pass limits of at least a few hundred bytes so the marker fits.
pub(crate) fn bounded_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let marker = |omitted: usize| {
        format!(" [... truncated by ContextWake: {omitted} more bytes not included]")
    };
    let mut cut = max_bytes.saturating_sub(marker(text.len()).len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut bounded = String::with_capacity(max_bytes);
    bounded.push_str(&text[..cut]);
    bounded.push_str(&marker(text.len() - cut));
    bounded
}

/// Like [`bounded_text`], but counts characters (JSON Schema `maxLength`).
pub(crate) fn bounded_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let marker = |omitted: usize| {
        format!(" [... truncated by ContextWake: {omitted} more characters not included]")
    };
    let keep = max_chars.saturating_sub(marker(total).chars().count());
    let mut bounded = text.chars().take(keep).collect::<String>();
    bounded.push_str(&marker(total - keep));
    bounded
}

/// Bounds already-redacted text to `max_chars`, re-scanning the cut text and
/// reporting whether that scan redacted anything.
fn fit_redacted(scanner: &SecretScanner, text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let report = scanner.redact(&bounded_chars(text, max_chars));
    let redacted = report.redacted();
    let fitted = if report.text.chars().count() <= max_chars {
        report.text
    } else {
        report.text.chars().take(max_chars).collect()
    };
    (fitted, redacted)
}

pub(crate) fn atomic_write(path: &Path, content: &[u8], overwrite: bool) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        ContextWakeError::UnsafePath(format!("{} has no parent directory", path.display()))
    })?;
    std::fs::create_dir_all(parent).at(parent)?;
    if path.exists() && !overwrite {
        return Err(ContextWakeError::InvalidData(format!(
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
        return Err(ContextWakeError::UnsafePath(format!(
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
        .map_err(|error| ContextWakeError::Io {
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
        return Err(ContextWakeError::UnsafePath(format!(
            "{} is outside managed checkpoint storage or is a symlink",
            sanitize_terminal(&path.display().to_string())
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TrustState, ValidationStatus};

    fn service(root: &Path) -> (CheckpointService, Store, Workspace) {
        let paths = AppPaths::from_root(root.join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        // An ordinary (non-Git) directory keeps these tests independent of how
        // a missing Git executable is reported.
        let workspace_path = root.join("workspace");
        std::fs::create_dir_all(&workspace_path).expect("workspace");
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: workspace_path,
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
        let git = GitClient::new(std::time::Duration::from_secs(5));
        (
            CheckpointService::new(store.clone(), paths, git),
            store,
            workspace,
        )
    }

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

    /// Re-points a checkpoint's index row at `path`.
    fn repoint_index(store: &Store, checkpoint: &Checkpoint, path: &Path) {
        store
            .delete_checkpoint_index(&checkpoint.id.to_string())
            .expect("remove row");
        store
            .insert_checkpoint_index(
                checkpoint.id,
                checkpoint.workspace_id,
                None,
                None,
                None,
                None,
                None,
                &checkpoint.objective,
                path,
                checkpoint.redaction_status,
                checkpoint.created_at,
            )
            .expect("re-insert row");
    }

    fn assert_recovery_removes_only_the_row(
        service: &CheckpointService,
        store: &Store,
        checkpoint: &Checkpoint,
        foreign: &Path,
    ) {
        let id = checkpoint.id.to_string();
        let listing = service.list_with_unreadable().expect("listing");
        assert_eq!(listing.unreadable.len(), 1);
        assert!(
            listing.unreadable[0]
                .reason
                .contains("outside managed checkpoint storage or is a symlink"),
            "{}",
            listing.unreadable[0].reason
        );
        assert_eq!(
            listing.unreadable[0].recovery_command(),
            format!("ctx checkpoint delete {id} --yes")
        );
        service.delete(&id).expect("the advertised recovery works");
        assert!(
            foreign.exists(),
            "a file outside managed storage was deleted"
        );
        assert!(store.checkpoint_path(&id).is_err(), "index row remains");
        assert!(
            service
                .list_with_unreadable()
                .expect("listing")
                .unreadable
                .is_empty()
        );
    }

    #[test]
    fn deleting_an_entry_outside_managed_storage_removes_only_its_index_row() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Moved state".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        // For example a CONTEXTWAKE_HOME copied elsewhere with the old copy kept.
        let managed = store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("path");
        let foreign = root
            .path()
            .join("old-state")
            .join(format!("{}.json", checkpoint.id));
        std::fs::create_dir_all(foreign.parent().expect("parent")).expect("old state");
        std::fs::rename(&managed, &foreign).expect("move file");
        repoint_index(&store, &checkpoint, &foreign);
        assert_recovery_removes_only_the_row(&service, &store, &checkpoint, &foreign);
    }

    #[cfg(unix)]
    #[test]
    fn deleting_an_entry_whose_file_is_a_symlink_keeps_the_link_target() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Linked".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        let managed = store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("path");
        let target = root.path().join("target.json");
        std::fs::rename(&managed, &target).expect("move file");
        std::os::unix::fs::symlink(&target, &managed).expect("symlink");
        assert_recovery_removes_only_the_row(&service, &store, &checkpoint, &target);
        assert!(
            std::fs::symlink_metadata(&managed).is_ok(),
            "symlink was touched"
        );
    }

    #[test]
    fn checkpoint_redacts_secrets_before_persistence() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Do work with sk-abcdefghijklmnopqrstuvwxyz".into(),
                    user_notes: Some("token=super-secret-value".into()),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        assert_eq!(checkpoint.redaction_status, RedactionStatus::Redacted);
        assert!(checkpoint.objective.contains("[REDACTED]"));
        let path = store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("indexed path");
        let persisted = std::fs::read_to_string(path).expect("persisted checkpoint");
        assert!(!persisted.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(!persisted.contains("super-secret-value"));
    }

    #[test]
    fn free_text_is_bounded_to_the_published_schema_limits() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, _, workspace) = service(root.path());
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "x".repeat(TEXT_FIELD_MAX_CHARS + 10_000),
                    completed: vec!["é".repeat(TEXT_FIELD_MAX_CHARS * 2)],
                    validation_results: vec![ValidationResult {
                        command: vec!["tool".into(), "a".repeat(COMMAND_ITEM_MAX_CHARS * 3)],
                        status: ValidationStatus::Passed,
                        observed_at: Utc::now(),
                        summary: "exit status 0".into(),
                    }],
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        assert!(checkpoint.objective.chars().count() <= TEXT_FIELD_MAX_CHARS);
        assert!(
            checkpoint
                .objective
                .ends_with("more characters not included]")
        );
        assert!(checkpoint.completed[0].chars().count() <= TEXT_FIELD_MAX_CHARS);
        let argument = &checkpoint.validation_results[0].command[1];
        assert!(argument.chars().count() <= COMMAND_ITEM_MAX_CHARS);
        assert!(argument.contains("[... truncated by ContextWake:"));
    }

    #[test]
    fn objective_of_only_terminal_control_data_is_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let result = service.create(
            &workspace,
            None,
            None,
            CheckpointInput {
                objective: "\u{1b}[31m\u{7}".into(),
                ..CheckpointInput::default()
            },
        );
        assert!(matches!(result, Err(ContextWakeError::InvalidData(_))));
        assert!(store.list_checkpoint_paths().expect("index").is_empty());
    }

    #[test]
    fn seeding_from_a_checkpoint_does_not_carry_validation_results() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, _, workspace) = service(root.path());
        let validated = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Ship CSV export".into(),
                    pending_tasks: vec!["Quoting".into()],
                    validation_results: vec![ValidationResult {
                        command: vec!["cargo".into(), "test".into()],
                        status: ValidationStatus::Passed,
                        observed_at: Utc::now(),
                        summary: "exit status 0".into(),
                    }],
                    ..CheckpointInput::default()
                },
            )
            .expect("validated checkpoint");
        assert_eq!(validated.validation_results.len(), 1);

        let seeded = CheckpointInput::from_checkpoint(&validated);
        assert_eq!(seeded.objective, "Ship CSV export");
        assert_eq!(seeded.pending_tasks, ["Quoting"]);
        assert!(seeded.validation_results.is_empty());
    }

    #[test]
    fn bounded_helpers_respect_limits_and_char_boundaries() {
        assert_eq!(bounded_text("short", 1024), "short");
        let text = "é".repeat(4_000);
        let bounded = bounded_text(&text, 1_000);
        assert!(bounded.len() <= 1_000);
        assert!(bounded.ends_with("more bytes not included]"));
        let chars = bounded_chars(&text, 500);
        assert_eq!(chars.chars().count(), 500);
        assert!(chars.ends_with("more characters not included]"));
    }

    #[test]
    fn checkpoint_without_a_git_executable_has_no_git_state() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths::from_root(root.path().join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        let workspace_path = root.path().join("workspace");
        std::fs::create_dir_all(&workspace_path).expect("workspace");
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: workspace_path,
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
        let git = GitClient::with_executable(
            "definitely-missing-git",
            std::time::Duration::from_millis(10),
        );
        let service = CheckpointService::new(store, paths, git);
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Do work with sk-abcdefghijklmnopqrstuvwxyz".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint without Git");
        // Without Git the workspace is an ordinary directory; doctor reports the missing Git.
        assert!(checkpoint.git.is_none() && checkpoint.files_changed.is_empty());
        assert!(
            !checkpoint
                .objective
                .contains("sk-abcdefghijklmnopqrstuvwxyz")
        );
        assert_eq!(checkpoint.redaction_status, RedactionStatus::Redacted);
    }

    fn excerpt(requests: Vec<String>) -> ConversationExcerpt {
        ConversationExcerpt {
            agent_id: "claude".into(),
            provider_session_id: "session\n42".into(),
            session_title: Some("Fix\tparser".into()),
            captured_at: Utc::now(),
            last_activity_at: None,
            user_requests: requests,
            last_agent_message: Some(format!(
                "done with password=hunter2hunter2 {}",
                "y".repeat(5_000)
            )),
            truncated: false,
        }
    }

    #[test]
    fn conversation_excerpts_are_redacted_bounded_and_written_as_schema_v3() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let requests = (1..=7)
            .map(|index| format!("request {index}"))
            .chain([
                "token=super-secret-value \u{1b}[31mred".to_string(),
                "   ".into(),
            ])
            .collect();
        let checkpoint = service
            .create_with_conversation(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Ship the parser".into(),
                    ..CheckpointInput::default()
                },
                Some(excerpt(requests)),
            )
            .expect("checkpoint");
        assert_eq!(checkpoint.schema_version, CHECKPOINT_SCHEMA_VERSION);
        assert_eq!(checkpoint.redaction_status, RedactionStatus::Redacted);
        let conversation = checkpoint.conversation.as_ref().expect("excerpt kept");
        assert_eq!(conversation.provider_session_id, "session 42");
        assert_eq!(conversation.session_title.as_deref(), Some("Fix parser"));
        assert_eq!(
            conversation.user_requests.len(),
            ConversationExcerpt::MAX_USER_REQUESTS
        );
        assert_eq!(conversation.user_requests[0], "request 4");
        let newest = conversation.user_requests.last().expect("newest");
        assert!(newest.contains("[REDACTED]") && !newest.contains('\u{1b}'));
        let answer = conversation.last_agent_message.as_deref().expect("answer");
        assert!(answer.chars().count() <= ConversationExcerpt::MAX_AGENT_MESSAGE_CHARS);
        assert!(!answer.contains("hunter2hunter2"));
        assert!(conversation.truncated);

        let persisted = std::fs::read_to_string(
            store
                .checkpoint_path(&checkpoint.id.to_string())
                .expect("path"),
        )
        .expect("file");
        assert!(persisted.contains("\"schema_version\": 3"));
        assert!(!persisted.contains("super-secret-value"));
        assert_eq!(
            service.read(&checkpoint.id.to_string()).expect("read back"),
            checkpoint
        );

        // An excerpt without any visible text, or without a usable id, is dropped.
        for unusable in [
            ConversationExcerpt {
                last_agent_message: None,
                ..excerpt(vec!["  ".into()])
            },
            ConversationExcerpt {
                provider_session_id: "  ".into(),
                ..excerpt(vec!["request".into()])
            },
            ConversationExcerpt {
                agent_id: "--bad agent".into(),
                ..excerpt(vec!["request".into()])
            },
        ] {
            let checkpoint = service
                .create_with_conversation(
                    &workspace,
                    None,
                    None,
                    CheckpointInput {
                        objective: "Ship".into(),
                        ..CheckpointInput::default()
                    },
                    Some(unusable),
                )
                .expect("checkpoint");
            assert!(checkpoint.conversation.is_none());
        }
    }

    #[test]
    fn readers_accept_checkpoint_schemas_one_to_three_only() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, store, workspace) = service(root.path());
        let checkpoint = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Versioned".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        assert!(checkpoint.conversation.is_none());
        let path = store
            .checkpoint_path(&checkpoint.id.to_string())
            .expect("path");
        let original: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("file")).expect("json");
        assert!(
            original.get("conversation").is_none(),
            "absent when not captured"
        );
        for version in [1, 2, 3, 4] {
            let mut value = original.clone();
            value["schema_version"] = serde_json::json!(version);
            std::fs::write(&path, serde_json::to_vec(&value).expect("encode")).expect("write");
            let result = service.read(&checkpoint.id.to_string());
            if version <= 3 {
                assert_eq!(result.expect("supported").schema_version, version);
            } else {
                assert!(
                    matches!(result, Err(ContextWakeError::InvalidData(message)) if message.contains("unsupported schema version 4"))
                );
            }
        }
    }

    #[test]
    fn seeding_never_carries_an_earlier_conversation_excerpt() {
        let root = tempfile::tempdir().expect("tempdir");
        let (service, _, workspace) = service(root.path());
        let earlier = service
            .create_with_conversation(
                &workspace,
                None,
                None,
                CheckpointInput {
                    objective: "Earlier".into(),
                    ..CheckpointInput::default()
                },
                Some(excerpt(vec!["old request".into()])),
            )
            .expect("earlier");
        assert!(earlier.conversation.is_some());
        let seeded = service
            .create(
                &workspace,
                None,
                None,
                CheckpointInput::from_checkpoint(&earlier),
            )
            .expect("seeded");
        assert_eq!(seeded.objective, "Earlier");
        assert!(seeded.conversation.is_none());
    }
}
