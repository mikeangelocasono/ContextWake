use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AgentDeckError, IoContext, Result};
use crate::git::GitClient;
use crate::model::{TrustState, Workspace};
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

pub fn detect_workspace(path: &Path, git: &GitClient, trust: TrustState) -> Result<Workspace> {
    let canonical = path.canonicalize().at(path)?;
    if !canonical.is_dir() {
        return Err(AgentDeckError::UnsafePath(format!(
            "{} is not a directory",
            canonical.display()
        )));
    }
    let git_root = git.repository_root(&canonical)?;
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

pub fn load_project_config(workspace: &Workspace) -> Result<Option<ProjectConfig>> {
    let path = workspace.path.join(".agentdeck").join("project.toml");
    if !path.exists() {
        return Ok(None);
    }
    if workspace.trust_state != TrustState::Trusted {
        return Err(AgentDeckError::UntrustedConfiguration(format!(
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
        return Err(AgentDeckError::UnsafePath(format!(
            "project configuration escapes the workspace or is a symlink: {}",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(&path).at(&path)?;
    let mut config: ProjectConfig = toml::from_str(&content)
        .map_err(|error| AgentDeckError::Configuration(format!("{}: {error}", path.display())))?;
    if config.schema_version != 1 {
        return Err(AgentDeckError::Configuration(format!(
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
        return Err(AgentDeckError::UnsafePath(
            instructions.display().to_string(),
        ));
    }
    if config.validation.len() > 32 {
        return Err(AgentDeckError::Configuration(
            "at most 32 validation commands are allowed".into(),
        ));
    }
    for command in &config.validation {
        if command.executable.trim().is_empty() {
            return Err(AgentDeckError::Configuration(
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
            return Err(AgentDeckError::Configuration(
                "validation commands contain control characters".into(),
            ));
        }
    }
    Ok(Some(config))
}

pub fn load_project_instructions(workspace: &Workspace) -> Result<Option<String>> {
    if workspace.trust_state != TrustState::Trusted {
        return Ok(None);
    }
    let Some(config) = load_project_config(workspace)? else {
        return Ok(None);
    };
    let Some(relative) = config.instructions_file else {
        return Ok(None);
    };
    let path = workspace.path.join(relative);
    let metadata = std::fs::symlink_metadata(&path).at(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AgentDeckError::UnsafePath(format!(
            "project instructions must be a regular file, not a symlink: {}",
            path.display()
        )));
    }
    if metadata.len() > 65_536 {
        return Err(AgentDeckError::InvalidData(format!(
            "project instructions exceed the 64 KiB capture limit: {}",
            path.display()
        )));
    }
    let canonical_workspace = workspace.path.canonicalize().at(&workspace.path)?;
    let canonical_path = path.canonicalize().at(&path)?;
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(AgentDeckError::UnsafePath(format!(
            "project instructions escape the workspace: {}",
            path.display()
        )));
    }
    std::fs::read_to_string(&path).at(&path).map(Some)
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
        std::fs::create_dir(root.path().join(".agentdeck")).expect("config directory");
        std::fs::write(
            root.path().join(".agentdeck/project.toml"),
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
}
