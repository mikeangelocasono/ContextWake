mod claude;
mod codex;
mod common;
mod copilot;
mod cursor;
mod discovery;
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

use discovery::DiscoveryContext;

pub use claude::ClaudeAdapter;
pub use copilot::GitHubCopilotAdapter;
pub use cursor::CursorAdapter;

/// Message of the trait's default `list_sessions`: the adapter has no
/// session-listing implementation.
const NO_SESSION_LISTING: &str = "does not expose a supported session-listing interface";
/// Message of the trait's default `available_models`: the adapter has no
/// model-catalog implementation.
const NO_MODEL_CATALOG: &str = "does not expose a supported model catalog";

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
            "{} {NO_SESSION_LISTING}",
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
            "{} {NO_MODEL_CATALOG}",
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
    /// Resolves every adapter's executable once, from the process `PATH`,
    /// working directory and `CONTEXTWAKE_*_BIN` overrides. Nothing is run.
    pub fn discover() -> Self {
        Self::discover_in(&DiscoveryContext::from_environment())
    }

    pub(crate) fn discover_in(context: &DiscoveryContext) -> Self {
        Self {
            adapters: vec![
                Arc::new(CodexAdapter::discover_in(context)),
                Arc::new(ClaudeAdapter::discover_in(context)),
                Arc::new(GitHubCopilotAdapter::discover_in(context)),
                Arc::new(CursorAdapter::discover_in(context)),
                Arc::new(OpenCodeAdapter::discover_in(context)),
                Arc::new(GeminiAdapter::discover_in(context)),
                Arc::new(KiroAdapter::discover_in(context)),
                Arc::new(KimiAdapter::discover_in(context)),
                Arc::new(GrokAdapter::discover_in(context)),
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
    use crate::provider::discovery::commands_built_on_this_thread;
    use crate::provider::discovery::tests::{executable, git_work_tree};

    const OVERRIDES: [&str; 9] = [
        "CONTEXTWAKE_CODEX_BIN",
        "CONTEXTWAKE_CLAUDE_BIN",
        "CONTEXTWAKE_COPILOT_BIN",
        "CONTEXTWAKE_CURSOR_BIN",
        "CONTEXTWAKE_OPENCODE_BIN",
        "CONTEXTWAKE_GEMINI_BIN",
        "CONTEXTWAKE_KIRO_BIN",
        "CONTEXTWAKE_KIMI_BIN",
        "CONTEXTWAKE_GROK_BIN",
    ];

    /// A registry in which no agent can be found: empty `PATH`, no overrides.
    fn registry_without_agents(root: &Path) -> AgentRegistry {
        AgentRegistry::discover_in(&DiscoveryContext::new(Some(root), Some(root), Vec::new()))
    }

    #[test]
    fn repository_local_agent_binary_is_not_a_safe_implicit_candidate() {
        let root = tempfile::tempdir().expect("root");
        let workspace = git_work_tree(&root.path().join("workspace"));
        let hostile = executable(&workspace.join("codex"));
        let trusted = executable(&root.path().join("system-bin").join("codex"));
        let context = DiscoveryContext::new(Some(&workspace), Some(root.path()), Vec::new());

        assert!(!context.is_safe_executable(&hostile));
        assert!(context.is_safe_executable(&trusted));
    }

    #[test]
    fn missing_agents_never_build_a_command() {
        // A missing agent used to become a fixed, creatable path under C:\
        // that detection executed (binary planting).
        let root = tempfile::tempdir().expect("root");
        let home = root.path().join("profile");
        let workspace = root.path().join("workspace");
        let handoff = root.path().join("handoff");
        for directory in [&home, &workspace, &handoff] {
            std::fs::create_dir_all(directory).expect("fixture directory");
        }
        std::fs::write(handoff.join("context.md"), "# Context").expect("context");
        let registry = registry_without_agents(root.path());
        let before = commands_built_on_this_thread();

        for (adapter, variable) in registry.implemented().zip(OVERRIDES) {
            for agent_home in [None, Some(home.as_path())] {
                let health = adapter.detect(agent_home).expect("detection never fails");
                assert!(!health.installed, "{}", adapter.id());
                assert!(health.executable.is_none(), "{}", adapter.id());
                assert!(health.version.is_none(), "{}", adapter.id());
                assert_eq!(health.auth_state, AuthState::Unknown);
                assert!(
                    health.message.contains(variable),
                    "{}: {}",
                    adapter.id(),
                    health.message
                );
            }
            let _ = adapter.auth_status(&home);
            for result in [
                adapter.login(&home, false).map(|_| ()),
                adapter.logout(&home),
                adapter.resume(&home, &workspace, "session-123"),
                adapter.start_with_handoff(&home, &workspace, &handoff, None, None),
            ] {
                let error = result.expect_err("a missing agent cannot be started");
                assert!(
                    matches!(error, ContextWakeError::CapabilityUnavailable(_)),
                    "{}: {error}",
                    adapter.id()
                );
            }
            let _ = adapter.list_sessions(&home, &workspace, 10);
            let _ = adapter.available_models(&home, Some(&workspace), None);
            assert!(
                adapter.acp_transport(Some(&home)).is_none(),
                "{}",
                adapter.id()
            );
        }
        assert_eq!(
            commands_built_on_this_thread(),
            before,
            "no command may be built for an agent that was not found"
        );
        let detected = registry.detect_all(Some(("codex", home.as_path())));
        assert_eq!(detected.len(), 9);
        assert!(
            detected
                .iter()
                .all(|health| health.as_ref().is_ok_and(|health| !health.installed))
        );
    }

    #[test]
    fn launch_errors_name_the_override_variable() {
        let root = tempfile::tempdir().expect("root");
        let registry = registry_without_agents(root.path());
        let codex = registry.get("codex").expect("codex");
        let error = codex.login(root.path(), false).expect_err("missing agent");
        let message = error.to_string();
        assert!(message.contains("could not be found"), "{message}");
        assert!(message.contains("CONTEXTWAKE_CODEX_BIN"), "{message}");
    }

    #[test]
    fn no_placeholder_executable_path_remains_in_sources() {
        // Built at run time so this test does not match itself.
        let needle = ["__contextwake", "_missing__"].concat();
        let mut pending = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut scanned = 0;
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read source directory") {
                let path = entry.expect("source entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    scanned += 1;
                    let source = std::fs::read_to_string(&path).expect("read source file");
                    assert!(
                        !source.contains(&needle),
                        "{} still contains a placeholder executable path",
                        path.display()
                    );
                }
            }
        }
        assert!(scanned > 10, "the source tree was not found");
    }

    #[test]
    fn capability_flags_match_adapter_implementations() {
        let root = tempfile::tempdir().expect("root");
        let home = root.path().join("profile");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&workspace).expect("workspace");

        // A capability flag must admit exactly the requests the adapter can
        // serve, under the test its callers use: `ctx session sync` gates on
        // `is_available()`, model catalogs are used only when `Verified`, and
        // the TUI's "Supports ACP" filter uses `is_available()`.
        for adapter in registry_without_agents(root.path()).implemented() {
            let capabilities = adapter.capabilities();
            let listing_implemented = !matches!(
                adapter.list_sessions(&home, &workspace, 1),
                Err(ContextWakeError::CapabilityUnavailable(message)) if message.contains(NO_SESSION_LISTING)
            );
            assert_eq!(
                capabilities.session_listing.is_available(),
                listing_implemented,
                "{} session_listing",
                adapter.id()
            );
            let catalog_implemented = !matches!(
                adapter.available_models(&home, Some(&workspace), None),
                Err(ContextWakeError::CapabilityUnavailable(message)) if message.contains(NO_MODEL_CATALOG)
            );
            assert_eq!(
                capabilities.available_models.support == crate::model::CapabilitySupport::Verified,
                catalog_implemented,
                "{} available_models",
                adapter.id()
            );
        }

        let mut context = DiscoveryContext::new(Some(root.path()), Some(root.path()), Vec::new());
        for variable in OVERRIDES {
            context = context.with_override(variable, "contextwake-test-unused-agent");
        }
        for adapter in AgentRegistry::discover_in(&context).implemented() {
            assert_eq!(
                adapter.capabilities().acp.is_available(),
                adapter.acp_transport(None).is_some(),
                "{} acp",
                adapter.id()
            );
        }
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
