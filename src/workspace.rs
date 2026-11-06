use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::git::GitClient;
use crate::model::{TrustState, Workspace};
use crate::paths::{canonicalize_simplified, simplify_path};
use crate::security::{sanitize_terminal, validate_local_name};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub schema_version: u32,
    pub name: Option<String>,
    #[serde(alias = "preferred_provider")]
    pub preferred_agent: Option<String>,
    pub preferred_model_provider: Option<String>,
    pub preferred_model: Option<String>,
    pub preferred_profile: Option<String>,
    pub validation: Vec<CommandSpec>,
    pub instructions_file: Option<PathBuf>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            name: None,
            preferred_agent: None,
            preferred_model_provider: None,
            preferred_model: None,
            preferred_profile: None,
            validation: Vec::new(),
            instructions_file: None,
        }
    }
}

/// Project metadata read where a broken repository file must not stop the
/// command (registration, `ctx status`, the TUI, checkpoint capture).
#[derive(Clone, Debug, Default)]
pub struct ProjectMetadata {
    /// The trusted project configuration, when it exists and is valid.
    pub config: Option<ProjectConfig>,
    /// Why the project configuration was ignored, when it was.
    pub warning: Option<String>,
}

/// Inspects `path` as a workspace: the top level of the Git work tree that
/// contains it, or the directory itself. Paths are canonical and in the
/// conventional (non-verbatim) form that agents, Git and people expect.
///
/// Git is optional. When it cannot be run, the directory is registered as an
/// ordinary workspace and status/checkpoints report Git as unavailable.
pub fn detect_workspace(path: &Path, git: &GitClient, trust: TrustState) -> Result<Workspace> {
    let canonical = canonical_workspace_directory(path)?;
    let git_root = git
        .repository_root(&canonical)
        .ok()
        .flatten()
        .map(|root| canonicalize_simplified(&root).unwrap_or_else(|_| simplify_path(&root)));
    let root = git_root.clone().unwrap_or(canonical);
    let display_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_terminal)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| root.display().to_string());
    let now = Utc::now();
    Ok(Workspace {
        id: Uuid::new_v4(),
        path: root,
        display_name,
        trust_state: trust,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root,
        created_at: now,
        last_opened_at: now,
    })
}

/// Canonical, conventional form of an existing directory, with a specific
/// error when it does not exist (a mistyped path, a deleted or renamed
/// project, or an unplugged drive).
pub fn canonical_workspace_directory(path: &Path) -> Result<PathBuf> {
    let canonical = match canonicalize_simplified(path) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ContextWakeError::WorkspaceNotFound(format!(
                "directory {} does not exist or is not reachable",
                sanitize_terminal(&path.display().to_string())
            )));
        }
        Err(source) => {
            return Err(ContextWakeError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !canonical.is_dir() {
        return Err(ContextWakeError::UnsafePath(format!(
            "{} is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Reads trusted project metadata without failing: an invalid, unsafe or
/// unreadable `project.toml` is reported as a warning and ignored. Explicit
/// validation (`ctx workspace validate`, `checkpoint create --validate`)
/// keeps using the strict [`load_project_config`].
pub fn read_project_metadata(workspace: &Workspace) -> ProjectMetadata {
    if workspace.trust_state != TrustState::Trusted {
        return ProjectMetadata::default();
    }
    match load_project_config(workspace) {
        Ok(config) => ProjectMetadata {
            config,
            warning: None,
        },
        Err(error) => ProjectMetadata {
            config: None,
            warning: Some(ignored_project_config_warning(&error)),
        },
    }
}

fn ignored_project_config_warning(error: &ContextWakeError) -> String {
    format!(
        "ignored the trusted project configuration ({}). Fix it, then run 'ctx workspace validate' to check it.",
        sanitize_terminal(&error.to_string())
    )
}

pub fn load_project_config(workspace: &Workspace) -> Result<Option<ProjectConfig>> {
    let current_path = workspace.path.join(".contextwake").join("project.toml");
    let legacy_path = workspace.path.join(".agentdeck").join("project.toml");
    let path = if current_path.exists() {
        current_path
    } else if legacy_path.exists() {
        legacy_path
    } else {
        return Ok(None);
    };
    if workspace.trust_state != TrustState::Trusted {
        return Err(ContextWakeError::UntrustedConfiguration(format!(
            "{} exists, but the workspace is not trusted; no commands were loaded",
            path.display()
        )));
    }
    let canonical_workspace = workspace.path.canonicalize().at(&workspace.path)?;
    let canonical_config = path.canonicalize().at(&path)?;
    if !canonical_config.starts_with(&canonical_workspace)
        || std::fs::symlink_metadata(&path)
            .at(&path)?
            .file_type()
            .is_symlink()
    {
        return Err(ContextWakeError::UnsafePath(format!(
            "project configuration escapes the workspace or is a symlink: {}",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(&path).at(&path)?;
    let mut config: ProjectConfig = toml::from_str(&content)
        .map_err(|error| ContextWakeError::Configuration(format!("{}: {error}", path.display())))?;
    if config.schema_version != 1 {
        return Err(ContextWakeError::Configuration(format!(
            "unsupported project config schema {}",
            config.schema_version
        )));
    }
    if let Some(name) = config.name.take() {
        config.name = Some(validate_local_name(&name)?);
    }
    if let Some(instructions) = &config.instructions_file
        && (instructions.is_absolute()
            || instructions
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)))
    {
        return Err(ContextWakeError::UnsafePath(
            instructions.display().to_string(),
        ));
    }
    if config.validation.len() > 32 {
        return Err(ContextWakeError::Configuration(
            "at most 32 validation commands are allowed".into(),
        ));
    }
    for command in &config.validation {
        if command.executable.trim().is_empty() {
            return Err(ContextWakeError::Configuration(
                "validation command executable cannot be empty".into(),
            ));
        }
        if command.executable.contains(['\n', '\r', '\0'])
            || command.executable.len() > 4_096
            || command.args.len() > 64
            || command
                .args
                .iter()
                .any(|arg| arg.contains(['\0']) || arg.len() > 16_384)
        {
            return Err(ContextWakeError::Configuration(
                "validation commands contain control characters".into(),
            ));
        }
    }
    Ok(Some(config))
}

/// Trusted project instructions captured into new checkpoints.
///
/// A broken `project.toml` or an unusable instructions file never blocks
/// checkpoint creation or profile switching: nothing is captured, and
/// [`read_project_instructions`] reports why. Nothing outside the workspace,
/// and no symlink, is ever read.
pub fn load_project_instructions(workspace: &Workspace) -> Result<Option<String>> {
    Ok(read_project_instructions(workspace).0)
}

/// Like [`load_project_instructions`], plus a warning that explains why no
/// instructions were captured when the trusted configuration or the
/// instructions file could not be used.
pub fn read_project_instructions(workspace: &Workspace) -> (Option<String>, Option<String>) {
    if workspace.trust_state != TrustState::Trusted {
        return (None, None);
    }
    let config = match load_project_config(workspace) {
        Ok(Some(config)) => config,
        Ok(None) => return (None, None),
        Err(error) => return (None, Some(ignored_project_config_warning(&error))),
    };
    let Some(relative) = config.instructions_file else {
        return (None, None);
    };
    match read_instructions_file(workspace, &relative) {
        Ok(instructions) => (Some(instructions), None),
        Err(error) => (
            None,
            Some(format!(
                "project instructions were not captured ({})",
                sanitize_terminal(&error.to_string())
            )),
        ),
    }
}

fn read_instructions_file(workspace: &Workspace, relative: &Path) -> Result<String> {
    let path = workspace.path.join(relative);
    let metadata = std::fs::symlink_metadata(&path).at(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ContextWakeError::UnsafePath(format!(
            "project instructions must be a regular file, not a symlink: {}",
            path.display()
        )));
    }
    if metadata.len() > 65_536 {
        return Err(ContextWakeError::InvalidData(format!(
            "project instructions exceed the 64 KiB capture limit: {}",
            path.display()
        )));
    }
    let canonical_workspace = workspace.path.canonicalize().at(&workspace.path)?;
    let canonical_path = path.canonicalize().at(&path)?;
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(ContextWakeError::UnsafePath(format!(
            "project instructions escape the workspace: {}",
            path.display()
        )));
    }
    std::fs::read_to_string(&path).at(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_argv_not_shell_text() {
        let config: ProjectConfig = toml::from_str(
            r#"schema_version = 1
               [[validation]]
               executable = "cargo"
               args = ["test", "--locked"]"#,
        )
        .expect("valid config");
        assert_eq!(config.validation[0].executable, "cargo");
        assert_eq!(config.validation[0].args, ["test", "--locked"]);
    }

    #[test]
    fn captures_bounded_trusted_project_instructions() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".contextwake")).expect("config directory");
        std::fs::write(
            root.path().join(".contextwake/project.toml"),
            "schema_version = 1\ninstructions_file = \"AGENTS.md\"\n",
        )
        .expect("config");
        std::fs::write(root.path().join("AGENTS.md"), "Keep changes local.\n")
            .expect("instructions");
        let git = GitClient::new(std::time::Duration::from_secs(1));
        let workspace =
            detect_workspace(root.path(), &git, TrustState::Trusted).expect("workspace");
        assert_eq!(
            load_project_instructions(&workspace).expect("instructions"),
            Some("Keep changes local.\n".into())
        );
    }

    #[test]
    fn legacy_project_config_remains_readable_after_rename() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".agentdeck")).expect("legacy config directory");
        std::fs::write(
            root.path().join(".agentdeck/project.toml"),
            "schema_version = 1\nname = \"Legacy workspace\"\n",
        )
        .expect("legacy config");
        let git = GitClient::new(std::time::Duration::from_secs(1));
        let workspace =
            detect_workspace(root.path(), &git, TrustState::Trusted).expect("workspace");

        let config = load_project_config(&workspace)
            .expect("legacy config")
            .expect("present");
        assert_eq!(config.name.as_deref(), Some("Legacy workspace"));
    }

    #[test]
    fn detected_paths_are_conventional_and_canonical() {
        let root = tempfile::tempdir().expect("tempdir");
        let repository = root.path().join("repo with spaces");
        std::fs::create_dir_all(repository.join("src")).expect("repository");
        let status = std::process::Command::new("git")
            .current_dir(&repository)
            .args(["init", "--quiet"])
            .stdin(std::process::Stdio::null())
            .status()
            .expect("Git must be installed for workspace tests");
        assert!(status.success());
        let git = GitClient::new(std::time::Duration::from_secs(10));

        let workspace = detect_workspace(&repository.join("src"), &git, TrustState::Untrusted)
            .expect("workspace");
        let expected = canonicalize_simplified(&repository).expect("canonical");
        assert_eq!(workspace.path, expected);
        assert_eq!(workspace.git_root.as_deref(), Some(expected.as_path()));
        let ordinary = detect_workspace(root.path(), &git, TrustState::Untrusted).expect("plain");
        for path in [&workspace.path, &ordinary.path] {
            assert!(
                !path.to_string_lossy().starts_with(r"\\?\"),
                "verbatim path {}",
                path.display()
            );
        }
    }

    #[test]
    fn missing_workspace_directory_has_a_specific_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let git = GitClient::new(std::time::Duration::from_secs(1));
        assert!(matches!(
            detect_workspace(&root.path().join("renamed"), &git, TrustState::Untrusted),
            Err(ContextWakeError::WorkspaceNotFound(_))
        ));
    }

    #[test]
    fn unusable_project_files_never_block_instruction_capture() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(root.path().join(".contextwake")).expect("config directory");
        let config = root.path().join(".contextwake/project.toml");
        let git = GitClient::new(std::time::Duration::from_secs(1));
        let workspace =
            detect_workspace(root.path(), &git, TrustState::Trusted).expect("workspace");

        std::fs::write(&config, "schema_version = 1\nfuture_setting = true\n")
            .expect("newer config");
        assert_eq!(
            load_project_instructions(&workspace).expect("capture"),
            None
        );
        let (instructions, warning) = read_project_instructions(&workspace);
        assert!(instructions.is_none());
        assert!(warning.is_some_and(|warning| warning.contains("future_setting")));
        assert!(
            read_project_metadata(&workspace)
                .warning
                .is_some_and(|warning| warning.contains("future_setting"))
        );
        assert!(
            load_project_config(&workspace).is_err(),
            "explicit validation stays strict"
        );

        std::fs::write(
            &config,
            "schema_version = 1\ninstructions_file = \"AGENTS.md\"\n",
        )
        .expect("config");
        let (instructions, warning) = read_project_instructions(&workspace);
        assert!(instructions.is_none());
        assert!(warning.is_some_and(|warning| warning.contains("not captured")));

        std::fs::write(root.path().join("AGENTS.md"), vec![b'x'; 65_537]).expect("large");
        let (instructions, warning) = read_project_instructions(&workspace);
        assert!(instructions.is_none());
        assert!(warning.is_some_and(|warning| warning.contains("64 KiB")));

        let untrusted = Workspace {
            trust_state: TrustState::Untrusted,
            ..workspace
        };
        assert_eq!(read_project_instructions(&untrusted), (None, None));
        assert!(read_project_metadata(&untrusted).config.is_none());
    }

    #[test]
    fn current_project_config_wins_over_legacy_file() {
        let root = tempfile::tempdir().expect("tempdir");
        for directory in [".agentdeck", ".contextwake"] {
            std::fs::create_dir(root.path().join(directory)).expect("config directory");
        }
        std::fs::write(
            root.path().join(".agentdeck/project.toml"),
            "schema_version = 1\nname = \"Legacy\"\n",
        )
        .expect("legacy config");
        std::fs::write(
            root.path().join(".contextwake/project.toml"),
            "schema_version = 1\nname = \"Current\"\n",
        )
        .expect("current config");
        let git = GitClient::new(std::time::Duration::from_secs(1));
        let workspace =
            detect_workspace(root.path(), &git, TrustState::Trusted).expect("workspace");

        let config = load_project_config(&workspace)
            .expect("current config")
            .expect("present");
        assert_eq!(config.name.as_deref(), Some("Current"));
    }
}
