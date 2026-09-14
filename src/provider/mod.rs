mod claude;
mod codex;
mod common;
mod copilot;
mod cursor;
mod gemini;
mod grok;
mod kimi;
mod kiro;
mod opencode;

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::error::{ContextWakeError, Result};
use crate::model::{
    AgentCapabilities, AgentHealth, AgentModel, AuthState, DiscoveredAgentSession, ModelProvider,
};

pub use claude::ClaudeAdapter;
pub use copilot::GitHubCopilotAdapter;
pub use cursor::CursorAdapter;

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
pub use grok::GrokAdapter;
pub use kimi::KimiAdapter;
pub use kiro::KiroAdapter;
pub use opencode::OpenCodeAdapter;

/// A fixed, shell-free ACP server launch contract supplied by an adapter.
/// `ContextWake` owns process lifetime when it uses this transport; provider
/// credentials remain in the provider's configured home.
#[derive(Clone, Debug)]
pub struct AcpTransport {
    executable: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
}

impl AcpTransport {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            environment: Vec::new(),
        }
    }

    #[must_use]
    pub fn argument(mut self, value: impl Into<OsString>) -> Self {
        self.arguments.push(value.into());
        self
    }

    #[must_use]
    pub fn environment(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.arguments).envs(self.environment.clone());
        command
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

/// Stable, compile-time interface for an AI coding CLI. Model backends are
/// reported separately through `model_providers`.
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }
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
    fn acp_transport(&self, _agent_home: Option<&Path>) -> Option<AcpTransport> {
        None
    }
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

#[derive(Clone)]
pub struct AgentRegistry {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl fmt::Debug for AgentRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRegistry")
            .field(
                "adapters",
                &self
                    .adapters
                    .iter()
                    .map(|adapter| adapter.id())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl AgentRegistry {
    pub fn discover() -> Self {
        Self {
            adapters: vec![
                Arc::new(CodexAdapter::discover()),
                Arc::new(ClaudeAdapter::discover()),
                Arc::new(GitHubCopilotAdapter::discover()),
                Arc::new(CursorAdapter::discover()),
                Arc::new(OpenCodeAdapter::discover()),
                Arc::new(GeminiAdapter::discover()),
                Arc::new(KiroAdapter::discover()),
                Arc::new(KimiAdapter::discover()),
                Arc::new(GrokAdapter::discover()),
            ],
        }
    }

    pub fn get(&self, id: &str) -> Result<&dyn AgentAdapter> {
        self.adapters
            .iter()
            .find(|adapter| {
                adapter.id().eq_ignore_ascii_case(id)
                    || adapter
                        .aliases()
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(id))
            })
            .map(Arc::as_ref)
            .ok_or_else(|| {
                ContextWakeError::CapabilityUnavailable(format!(
                    "agent {id} has no implemented adapter; run 'ctx agent list'"
                ))
            })
    }

    pub fn implemented(&self) -> impl Iterator<Item = &dyn AgentAdapter> {
        self.adapters.iter().map(Arc::as_ref)
    }

    /// Probe adapters with bounded parallelism while preserving registry order.
    /// A single slow CLI therefore cannot freeze every discovery result, and
    /// launching many Node-based agents at once cannot starve their timeouts.
    pub fn detect_all(&self, active_agent: Option<(&str, &Path)>) -> Vec<Result<AgentHealth>> {
        const MAX_CONCURRENT_PROBES: usize = 3;
        std::thread::scope(|scope| {
            let mut results = Vec::with_capacity(self.adapters.len());
            for adapters in self.adapters.chunks(MAX_CONCURRENT_PROBES) {
                let handles = adapters
                    .iter()
                    .map(|adapter| {
                        let home = active_agent.and_then(|(id, home)| {
                            adapter.id().eq_ignore_ascii_case(id).then_some(home)
                        });
                        scope.spawn(move || adapter.detect(home))
                    })
                    .collect::<Vec<_>>();
                results.extend(handles.into_iter().map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        Err(ContextWakeError::InvalidData(
                            "coding-agent probe worker panicked".into(),
                        ))
                    })
                }));
            }
            results
        })
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

    #[test]
    fn registry_uses_adapter_aliases_without_provider_branches() {
        let registry = AgentRegistry::discover();
        assert_eq!(registry.get("claude-code").expect("alias").id(), "claude");
        assert_eq!(
            registry.get("github-copilot").expect("stable id").id(),
            "github-copilot"
        );
        assert_eq!(registry.implemented().count(), 9);
    }

    #[test]
    fn acp_transport_builds_a_shell_free_command() {
        let transport = AcpTransport::new("provider")
            .argument("agent")
            .argument("stdio")
            .environment("PROVIDER_HOME", "isolated");
        assert_eq!(transport.executable(), Path::new("provider"));
        assert_eq!(transport.arguments(), ["agent", "stdio"]);
        let command = transport.command();
        assert_eq!(command.get_program(), "provider");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("agent"), std::ffi::OsStr::new("stdio")]
        );
    }
}
