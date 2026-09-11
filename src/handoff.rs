use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use crate::VERSION;
use crate::checkpoint::{CheckpointService, atomic_write};
use crate::error::{AgentDeckError, IoContext, Result};
use crate::model::{
    Checkpoint, ContentFile, HandoffManifest, HandoffRecord, HandoffSource, HandoffWorkspace,
    Workspace,
};
use crate::paths::AppPaths;
use crate::security::{
    SecretScanner, ensure_relative_payload_path, path_fingerprint, sanitize_terminal, sha256_bytes,
    validate_external_reference,
};
use crate::store::Store;

const HANDOFF_SCHEMA: &str = "1.1.0";
const MAX_CONTENT_FILES: usize = 16;
const MAX_CONTENT_FILE_BYTES: u64 = 1024 * 1024;
const MAX_HANDOFF_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct HandoffService {
    store: Store,
    paths: AppPaths,
}

impl HandoffService {
    pub fn new(store: Store, paths: AppPaths) -> Self {
        Self { store, paths }
    }

    pub fn create(&self, checkpoint: &Checkpoint, workspace: &Workspace) -> Result<HandoffRecord> {
        if checkpoint.workspace_id != workspace.id {
            return Err(AgentDeckError::InvalidData(
                "checkpoint belongs to a different workspace".into(),
            ));
        }
        let handoff_id = Uuid::new_v4();
        let markdown = render_context(checkpoint, workspace);
        let scan = SecretScanner::new().redact(&markdown);
        let was_redacted = scan.redacted();
        let context = scan.text;
        let git_json = serde_json::to_vec_pretty(&checkpoint.git).map_err(|error| {
            AgentDeckError::InvalidData(format!("could not serialize Git summary: {error}"))
        })?;
        let validation_json =
            serde_json::to_vec_pretty(&checkpoint.validation_results).map_err(|error| {
                AgentDeckError::InvalidData(format!("could not serialize validation: {error}"))
            })?;
        let content_files = vec![
            ContentFile {
                path: "context.md".into(),
                sha256: sha256_bytes(context.as_bytes()),
                media_type: "text/markdown".into(),
            },
            ContentFile {
                path: "git-summary.json".into(),
                sha256: sha256_bytes(&git_json),
                media_type: "application/json".into(),
            },
            ContentFile {
                path: "validation.json".into(),
                sha256: sha256_bytes(&validation_json),
                media_type: "application/json".into(),
            },
        ];
        let mut security_flags = vec!["commands_are_untrusted_notes_only".into()];
        if was_redacted || checkpoint.redaction_status == crate::model::RedactionStatus::Redacted {
            security_flags.push("secrets_redacted".into());
        }
        let manifest = HandoffManifest {
            schema_version: HANDOFF_SCHEMA.into(),
            handoff_id,
            created_at: Utc::now(),
            source_tool: format!("agentdeck/{VERSION}"),
            source: checkpoint.agent_id.as_ref().map(|agent_id| HandoffSource {
                agent_id: agent_id.clone(),
                model_provider_id: checkpoint.model_provider_id.clone(),
                model: checkpoint.model.clone(),
                profile_id: checkpoint.profile_id,
                session_id: checkpoint.session_id.map(|id| id.to_string()),
            }),
            source_provider: None,
            checkpoint_id: checkpoint.id,
            workspace: HandoffWorkspace {
                display_name: sanitize_terminal(&workspace.display_name),
                path_fingerprint: path_fingerprint(&workspace.path),
                branch: checkpoint.git.as_ref().and_then(|git| git.branch.clone()),
                commit: checkpoint
                    .git
                    .as_ref()
                    .and_then(|git| git.head_commit.clone()),
            },
            objective: sanitize_terminal(&checkpoint.objective),
            content_files,
            redactions: scan.counts,
            security_flags,
            extensions: BTreeMap::new(),
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
            AgentDeckError::InvalidData(format!("could not serialize handoff manifest: {error}"))
        })?;

        let temporary = tempfile::Builder::new()
            .prefix(".handoff-")
            .tempdir_in(self.paths.handoffs_dir())
            .at(self.paths.handoffs_dir())?;
        atomic_write(
            &temporary.path().join("context.md"),
            context.as_bytes(),
            false,
        )?;
        atomic_write(&temporary.path().join("git-summary.json"), &git_json, false)?;
        atomic_write(
            &temporary.path().join("validation.json"),
            &validation_json,
            false,
        )?;
        atomic_write(
            &temporary.path().join("manifest.json"),
            &manifest_json,
            false,
        )?;

        let temporary_path = temporary.keep();
        let storage_path = self.paths.handoffs_dir().join(handoff_id.to_string());
        std::fs::rename(&temporary_path, &storage_path).at(&storage_path)?;
        let record = HandoffRecord {
            id: handoff_id,
            checkpoint_id: checkpoint.id,
            schema_version: HANDOFF_SCHEMA.into(),
            storage_path: storage_path.clone(),
            sha256: sha256_bytes(&manifest_json),
            created_at: manifest.created_at,
        };
        if let Err(error) = self.store.insert_handoff(&record) {
            let _ = std::fs::remove_dir_all(storage_path);
            return Err(error);
        }
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<HandoffRecord>> {
        self.store.list_handoffs()
    }

    pub fn manifest(&self, reference: &str) -> Result<HandoffManifest> {
        let record = self.store.handoff(reference)?;
        let path = self.managed_payload_path(&record, "manifest.json")?;
        let bytes = std::fs::read(&path).at(&path)?;
        if sha256_bytes(&bytes) != record.sha256 {
            return Err(AgentDeckError::InvalidData(format!(
                "handoff {reference} manifest integrity check failed"
            )));
        }
        let manifest: HandoffManifest = serde_json::from_slice(&bytes).map_err(|error| {
            AgentDeckError::InvalidData(format!("invalid handoff manifest: {error}"))
        })?;
        if !is_supported_handoff_schema(&manifest.schema_version)
            || manifest.handoff_id != record.id
        {
            return Err(AgentDeckError::InvalidData(
                "handoff manifest schema or identity mismatch".into(),
            ));
        }
        Ok(manifest)
    }

    pub fn validate(&self, reference: &str) -> Result<HandoffManifest> {
        let record = self.store.handoff(reference)?;
        let manifest = self.manifest(reference)?;
        validate_manifest_strings(&manifest)?;
        let mut total_bytes = 0_u64;
        for content in &manifest.content_files {
            let path = self.managed_payload_path(&record, &content.path)?;
            let bytes = read_bounded_file(&path, MAX_CONTENT_FILE_BYTES)?;
            total_bytes =
                total_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if total_bytes > MAX_HANDOFF_BYTES {
                return Err(AgentDeckError::InvalidData(
                    "managed handoff exceeds the 4 MiB validation limit".into(),
                ));
            }
            if sha256_bytes(&bytes) != content.sha256 {
                return Err(AgentDeckError::InvalidData(format!(
                    "handoff content integrity check failed for {}",
                    content.path
                )));
            }
        }
        Ok(manifest)
    }

    pub fn context(&self, reference: &str) -> Result<String> {
        let record = self.store.handoff(reference)?;
        let manifest = self.validate(reference)?;
        let content = manifest
            .content_files
            .iter()
            .find(|entry| entry.path == "context.md")
            .ok_or_else(|| AgentDeckError::InvalidData("handoff has no context.md".into()))?;
        let path = self.managed_payload_path(&record, &content.path)?;
        let bytes = std::fs::read(&path).at(&path)?;
        if sha256_bytes(&bytes) != content.sha256 {
            return Err(AgentDeckError::InvalidData(
                "handoff context integrity check failed".into(),
            ));
        }
        Ok(sanitize_terminal(&String::from_utf8_lossy(&bytes)))
    }

    pub fn export(&self, reference: &str, destination: &Path, overwrite: bool) -> Result<PathBuf> {
        let record = self.store.handoff(reference)?;
        let manifest = self.validate(reference)?;
        if destination.exists() {
            if std::fs::symlink_metadata(destination)
                .at(destination)?
                .file_type()
                .is_symlink()
            {
                return Err(AgentDeckError::UnsafePath(format!(
                    "export destination is a symlink: {}",
                    destination.display()
                )));
            }
            if !destination.is_dir() {
                return Err(AgentDeckError::UnsafePath(format!(
                    "{} is not a directory",
                    destination.display()
                )));
            }
            if !overwrite {
                return Err(AgentDeckError::InvalidData(format!(
                    "{} already exists; pass --force to write into it",
                    destination.display()
                )));
            }
        } else {
            std::fs::create_dir_all(destination).at(destination)?;
        }
        let mut files = vec!["manifest.json".to_string()];
        files.extend(
            manifest
                .content_files
                .iter()
                .map(|entry| entry.path.clone()),
        );
        for relative in files {
            ensure_relative_payload_path(Path::new(&relative))?;
            let source = self.managed_payload_path(&record, &relative)?;
            let target = destination.join(&relative);
            let bytes = std::fs::read(&source).at(&source)?;
            atomic_write(&target, &bytes, overwrite)?;
        }
        Ok(destination.to_path_buf())
    }

    pub fn import_directory(&self, source: &Path) -> Result<HandoffRecord> {
        let source_metadata = std::fs::symlink_metadata(source).at(source)?;
        if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
            return Err(AgentDeckError::UnsafePath(format!(
                "{} must be a real directory, not a symlink",
                source.display()
            )));
        }
        let manifest_path = source.join("manifest.json");
        let manifest_bytes = read_bounded_file(&manifest_path, MAX_CONTENT_FILE_BYTES)?;
        let manifest: HandoffManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|error| {
                AgentDeckError::InvalidData(format!(
                    "invalid handoff manifest {}: {error}",
                    manifest_path.display()
                ))
            })?;
        if !is_supported_handoff_schema(&manifest.schema_version) {
            return Err(AgentDeckError::InvalidData(format!(
                "unsupported handoff schema {}",
                manifest.schema_version
            )));
        }
        validate_manifest_strings(&manifest)?;
        let manifest_text = String::from_utf8(manifest_bytes.clone())
            .map_err(|_| AgentDeckError::InvalidData("manifest is not valid UTF-8".into()))?;
        if SecretScanner::new().redact(&manifest_text).redacted() {
            return Err(AgentDeckError::InvalidData(
                "manifest contains possible secret material; redact it before import".into(),
            ));
        }
        if manifest.content_files.len() > MAX_CONTENT_FILES {
            return Err(AgentDeckError::InvalidData(format!(
                "handoff contains more than {MAX_CONTENT_FILES} content files"
            )));
        }
        if !manifest
            .security_flags
            .iter()
            .any(|flag| flag == "commands_are_untrusted_notes_only")
        {
            return Err(AgentDeckError::InvalidData(
                "handoff does not declare commands as untrusted notes".into(),
            ));
        }

        let mut seen = BTreeSet::new();
        let mut payloads = Vec::with_capacity(manifest.content_files.len());
        let mut total_bytes = u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX);
        for content in &manifest.content_files {
            let relative = Path::new(&content.path);
            ensure_relative_payload_path(relative)?;
            if relative.components().count() != 1 {
                return Err(AgentDeckError::UnsafePath(format!(
                    "handoff content must use a top-level filename: {}",
                    content.path
                )));
            }
            if !seen.insert(content.path.clone()) {
                return Err(AgentDeckError::InvalidData(format!(
                    "duplicate handoff content path {}",
                    content.path
                )));
            }
            if !matches!(
                content.media_type.as_str(),
                "text/markdown" | "application/json"
            ) {
                return Err(AgentDeckError::InvalidData(format!(
                    "unsupported handoff media type {}",
                    content.media_type
                )));
            }
            let content_path = source.join(relative);
            let bytes = read_bounded_file(&content_path, MAX_CONTENT_FILE_BYTES)?;
            if sha256_bytes(&bytes) != content.sha256 {
                return Err(AgentDeckError::InvalidData(format!(
                    "handoff content integrity check failed for {}",
                    content.path
                )));
            }
            let text = String::from_utf8(bytes.clone()).map_err(|_| {
                AgentDeckError::InvalidData(format!("{} is not valid UTF-8", content.path))
            })?;
            if SecretScanner::new().redact(&text).redacted() {
                return Err(AgentDeckError::InvalidData(format!(
                    "{} contains possible secret material; redact it before import",
                    content.path
                )));
            }
            total_bytes =
                total_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if total_bytes > MAX_HANDOFF_BYTES {
                return Err(AgentDeckError::InvalidData(
                    "handoff exceeds the 4 MiB import limit".into(),
                ));
            }
            payloads.push((content.path.clone(), bytes));
        }
        if !seen.contains("context.md") {
            return Err(AgentDeckError::InvalidData(
                "handoff manifest must include context.md".into(),
            ));
        }

        let storage_path = self
            .paths
            .handoffs_dir()
            .join(manifest.handoff_id.to_string());
        if storage_path.exists() {
            return Err(AgentDeckError::InvalidData(format!(
                "handoff {} is already present",
                manifest.handoff_id
            )));
        }
        let temporary = tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(self.paths.handoffs_dir())
            .at(self.paths.handoffs_dir())?;
        atomic_write(
            &temporary.path().join("manifest.json"),
            &manifest_bytes,
            false,
        )?;
        for (relative, bytes) in payloads {
            atomic_write(&temporary.path().join(relative), &bytes, false)?;
        }
        let temporary_path = temporary.keep();
        std::fs::rename(&temporary_path, &storage_path).at(&storage_path)?;
        let record = HandoffRecord {
            id: manifest.handoff_id,
            // A portable checkpoint UUID is untrusted and must never be allowed
            // to collide with or imply ownership of a local checkpoint.
            checkpoint_id: Uuid::nil(),
            schema_version: manifest.schema_version,
            storage_path: storage_path.clone(),
            sha256: sha256_bytes(&manifest_bytes),
            created_at: manifest.created_at,
        };
        if let Err(error) = self.store.insert_handoff(&record) {
            let _ = std::fs::remove_dir_all(storage_path);
            return Err(error);
        }
        Ok(record)
    }

    fn managed_payload_path(&self, record: &HandoffRecord, relative: &str) -> Result<PathBuf> {
        let relative = Path::new(relative);
        ensure_relative_payload_path(relative)?;
        let canonical_base = self
            .paths
            .handoffs_dir()
            .canonicalize()
            .at(self.paths.handoffs_dir())?;
        let canonical_record = record
            .storage_path
            .canonicalize()
            .at(&record.storage_path)?;
        if !canonical_record.starts_with(&canonical_base)
            || std::fs::symlink_metadata(&record.storage_path)
                .at(&record.storage_path)?
                .file_type()
                .is_symlink()
        {
            return Err(AgentDeckError::UnsafePath(
                record.storage_path.display().to_string(),
            ));
        }
        let path = canonical_record.join(relative);
        let mut cursor = canonical_record.clone();
        for component in relative.components() {
            if let std::path::Component::Normal(component) = component {
                cursor.push(component);
                if std::fs::symlink_metadata(&cursor)
                    .at(&cursor)?
                    .file_type()
                    .is_symlink()
                {
                    return Err(AgentDeckError::UnsafePath(cursor.display().to_string()));
                }
            }
        }
        let canonical_path = path.canonicalize().at(&path)?;
        if !canonical_path.starts_with(&canonical_record) {
            return Err(AgentDeckError::UnsafePath(path.display().to_string()));
        }
        Ok(canonical_path)
    }
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path).at(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AgentDeckError::UnsafePath(format!(
            "{} must be a real file, not a symlink",
            path.display()
        )));
    }
    if metadata.len() > maximum {
        return Err(AgentDeckError::InvalidData(format!(
            "{} exceeds the {maximum} byte limit",
            path.display()
        )));
    }
    std::fs::read(path).at(path)
}

fn validate_manifest_strings(manifest: &HandoffManifest) -> Result<()> {
    if manifest.schema_version == "1.1.0" && manifest.source.is_none() {
        return Err(AgentDeckError::InvalidData(
            "AWHF 1.1 requires structured source agent metadata".into(),
        ));
    }
    let mut values = vec![
        manifest.source_tool.as_str(),
        manifest.objective.as_str(),
        manifest.workspace.display_name.as_str(),
    ];
    values.extend(manifest.source_provider.as_deref());
    if let Some(source) = &manifest.source {
        validate_external_reference(&source.agent_id)?;
        if let Some(value) = &source.model_provider_id {
            validate_external_reference(value)?;
        }
        if let Some(value) = &source.model {
            validate_external_reference(value)?;
        }
        if let Some(value) = &source.session_id {
            validate_external_reference(value)?;
        }
        values.push(source.agent_id.as_str());
        values.extend(source.model_provider_id.as_deref());
        values.extend(source.model.as_deref());
        values.extend(source.session_id.as_deref());
    }
    if let Some(value) = &manifest.source_provider {
        validate_external_reference(value)?;
    }
    values.extend(manifest.workspace.branch.as_deref());
    values.extend(manifest.workspace.commit.as_deref());
    values.extend(manifest.security_flags.iter().map(String::as_str));
    for content in &manifest.content_files {
        values.push(content.path.as_str());
        values.push(content.media_type.as_str());
    }
    if values
        .into_iter()
        .any(|value| sanitize_terminal(value) != value)
    {
        return Err(AgentDeckError::InvalidData(
            "handoff manifest contains terminal control data".into(),
        ));
    }
    Ok(())
}

fn is_supported_handoff_schema(value: &str) -> bool {
    matches!(value, "1.0.0" | "1.1.0")
}

fn render_context(checkpoint: &Checkpoint, workspace: &Workspace) -> String {
    let mut output = format!(
        "# Workspace Handoff\n\n> Generated from explicit local project state. This does not contain hidden reasoning or transferable provider session ownership. Commands are notes only.\n\n## Objective\n{}\n\n",
        sanitize_terminal(&checkpoint.objective)
    );
    if let Some(task) = &checkpoint.active_task {
        push_section(&mut output, "Current Work", std::slice::from_ref(task));
    }
    push_section(&mut output, "Completed", &checkpoint.completed);
    push_section(&mut output, "Decisions", &checkpoint.decisions);
    push_section(&mut output, "Pending", &checkpoint.pending_tasks);
    push_section(&mut output, "Known Issues", &checkpoint.known_issues);

    output.push_str("## Workspace\n");
    let _ = writeln!(
        output,
        "Name: {}",
        sanitize_terminal(&workspace.display_name)
    );
    output.push_str("Path: omitted from portable handoff; see manifest fingerprint\n\n");

    if let Some(git) = &checkpoint.git {
        output.push_str("## Git\n");
        let _ = write!(
            output,
            "Branch: {}\nState: {}\nFiles: {} staged, {} unstaged, {} untracked\nDiff summary: +{} -{}\nLatest commit: {}\n\n",
            git.branch.as_deref().unwrap_or("detached or unavailable"),
            if git.dirty { "dirty" } else { "clean" },
            git.staged_count,
            git.unstaged_count,
            git.untracked_count,
            git.additions,
            git.deletions,
            git.last_commit.as_deref().unwrap_or("unavailable")
        );
    }
    if !checkpoint.files_changed.is_empty() {
        output.push_str("## Files Modified\n");
        for path in &checkpoint.files_changed {
            let _ = writeln!(
                output,
                "- {}",
                sanitize_terminal(&path.display().to_string())
            );
        }
        output.push('\n');
    }
    output.push_str("## Validation\n");
    if checkpoint.validation_results.is_empty() {
        output.push_str("No validation commands were run by AgentDeck.\n\n");
    } else {
        for result in &checkpoint.validation_results {
            let _ = writeln!(
                output,
                "- {:?}: {}",
                result.status,
                sanitize_terminal(&result.summary)
            );
        }
        output.push('\n');
    }
    if let Some(notes) = &checkpoint.user_notes {
        output.push_str("## User Notes\n");
        output.push_str(&sanitize_terminal(notes));
        output.push_str("\n\n");
    }
    if let Some(instructions) = &checkpoint.project_instructions {
        output.push_str("## Project Instructions\n");
        output.push_str(&sanitize_terminal(instructions));
        output.push_str("\n\n");
    }
    output
}

fn push_section(output: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(output, "## {title}");
    for item in items {
        let _ = writeln!(output, "- {}", sanitize_terminal(item));
    }
    output.push('\n');
}

pub fn create_from_checkpoint_reference(
    service: &HandoffService,
    checkpoints: &CheckpointService,
    checkpoint_reference: &str,
    workspace: &Workspace,
) -> Result<HandoffRecord> {
    let checkpoint = checkpoints.read(checkpoint_reference)?;
    if checkpoint.workspace_id != workspace.id {
        return Err(AgentDeckError::InvalidData(
            "checkpoint belongs to a different workspace".into(),
        ));
    }
    service.create(&checkpoint, workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RedactionStatus, TrustState};

    #[test]
    fn handoff_omits_absolute_workspace_path_and_hidden_context_claims() {
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: PathBuf::from("/sensitive/client/repo"),
            display_name: "repo".into(),
            trust_state: TrustState::Trusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        };
        let checkpoint = Checkpoint {
            schema_version: 2,
            id: Uuid::new_v4(),
            workspace_id: workspace.id,
            session_id: None,
            agent_id: Some("codex".into()),
            model_provider_id: Some("openai".into()),
            model: None,
            profile_id: None,
            objective: "Continue implementation".into(),
            active_task: None,
            completed: Vec::new(),
            decisions: Vec::new(),
            pending_tasks: Vec::new(),
            known_issues: Vec::new(),
            user_notes: None,
            project_instructions: None,
            files_changed: Vec::new(),
            git: None,
            validation_results: Vec::new(),
            created_at: now,
            redaction_status: RedactionStatus::Clean,
        };
        let rendered = render_context(&checkpoint, &workspace);
        assert!(!rendered.contains("/sensitive/client/repo"));
        assert!(rendered.contains("does not contain hidden reasoning"));
    }
}
