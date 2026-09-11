mod claude;
mod codex;
mod gemini;
mod kiro;
mod opencode;

use std::path::Path;

use crate::error::{ContextWakeError, Result};
use crate::model::{
    AgentCapabilities, AgentHealth, AgentModel, AuthState, DiscoveredAgentSession, ModelProvider,
};

pub use claude::ClaudeAdapter;

#[cfg(not(windows))]
pub(crate) fn find_safe_on_path(executable_name: &str) -> Option<std::path::PathBuf> {
    let current = std::env::current_dir().ok()?.canonicalize().ok()?;
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(executable_name);
        if safe_executable_candidate(&candidate, &current) {
            return candidate.canonicalize().ok();
        }
    }
    None
}

fn safe_executable_candidate(candidate: &Path, current_workspace: &Path) -> bool {
    candidate.is_file()
        && !candidate.starts_with(current_workspace)
        && candidate
            .canonicalize()
            .is_ok_and(|resolved| !resolved.starts_with(current_workspace))
}
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use kiro::KiroAdapter;
pub use opencode::OpenCodeAdapter;

/// Stable, compile-time interface for an AI coding CLI. Model backends are
/// reported separately through `model_providers`.
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn model_providers(&self) -> Vec<ModelProvider>;
    fn accepts_model_provider(&self, provider_id: &str) -> bool {
        self.model_providers()
            .iter()
            .any(|candidate| candidate.id.eq_ignore_ascii_case(provider_id))
    }
    fn normalize_model_selection(
        &self,
        provider_id: Option<&str>,
        model: &str,
    ) -> Result<(Option<String>, String)> {
        Ok((provider_id.map(str::to_string), model.to_string()))
    }
    fn capabilities(&self) -> AgentCapabilities;
    fn detect(&self, agent_home: Option<&Path>) -> Result<AgentHealth>;
    fn auth_status(&self, agent_home: &Path) -> Result<AuthState>;
    fn initialize_profile_home(&self, agent_home: &Path) -> Result<()>;
    fn login(&self, agent_home: &Path, device_auth: bool) -> Result<AuthState>;
    fn logout(&self, agent_home: &Path) -> Result<()>;
    fn list_sessions(
        &self,
        _agent_home: &Path,
        _workspace: &Path,
        _max_count: usize,
    ) -> Result<Vec<DiscoveredAgentSession>> {
        Err(ContextWakeError::CapabilityUnavailable(format!(
            "{} does not expose a supported session-listing interface",
            self.display_name()
        )))
    }
    fn available_models(
        &self,
        _agent_home: &Path,
        _workspace: Option<&Path>,
        _provider_id: Option<&str>,
    ) -> Result<Vec<AgentModel>> {
        Err(ContextWakeError::CapabilityUnavailable(format!(
            "{} does not expose a supported model catalog",
            self.display_name()
        )))
    }
    fn resume(&self, agent_home: &Path, workspace: &Path, session_id: &str) -> Result<()>;
    fn start_with_handoff(
        &self,
        agent_home: &Path,
        workspace: &Path,
        handoff_directory: &Path,
        model_provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct AgentRegistry {
    codex: CodexAdapter,
    claude: ClaudeAdapter,
    gemini: GeminiAdapter,
    kiro: KiroAdapter,
    opencode: OpenCodeAdapter,
}

impl AgentRegistry {
    pub fn discover() -> Self {
        Self {
            codex: CodexAdapter::discover(),
            claude: ClaudeAdapter::discover(),
            gemini: GeminiAdapter::discover(),
            kiro: KiroAdapter::discover(),
            opencode: OpenCodeAdapter::discover(),
        }
    }

    pub fn get(&self, id: &str) -> Result<&dyn AgentAdapter> {
        match id.to_ascii_lowercase().as_str() {
            "codex" => Ok(&self.codex),
            "claude" | "claude-code" => Ok(&self.claude),
            "gemini" | "gemini-cli" => Ok(&self.gemini),
            "kiro" | "kiro-cli" => Ok(&self.kiro),
            "opencode" => Ok(&self.opencode),
            _ => Err(ContextWakeError::CapabilityUnavailable(format!(
                "agent {id} has no implemented adapter; run 'ctxwake agent list'"
            ))),
        }
    }

    pub fn implemented(&self) -> [&dyn AgentAdapter; 5] {
        [
            &self.codex,
            &self.claude,
            &self.gemini,
            &self.kiro,
            &self.opencode,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_local_agent_binary_is_not_a_safe_implicit_candidate() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let system = root.path().join("system-bin");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&system).expect("system bin");
        let hostile = workspace.join("codex");
        let trusted = system.join("codex");
        std::fs::write(&hostile, "hostile").expect("hostile binary fixture");
        std::fs::write(&trusted, "trusted").expect("trusted binary fixture");

        assert!(!safe_executable_candidate(&hostile, &workspace));
        assert!(safe_executable_candidate(&trusted, &workspace));
    }
}
