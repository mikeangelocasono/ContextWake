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

use chrono::{DateTime, Utc};

use crate::error::{ContextWakeError, Result};
use crate::model::{
    AgentCapabilities, AgentHealth, AgentModel, AuthState, ConversationExcerpt,
    DiscoveredAgentSession, ModelProvider,
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
/// Message of the trait's default `conversation_excerpt`.
const NO_CONVERSATION_EXCERPT: &str = "does not expose a readable conversation transcript";
/// Message of the trait's default `launch_with_handoff` for a resume.
const NO_RESUME_WITH_HANDOFF: &str =
    "cannot deliver a handoff into a natively resumed session; start a new session instead";

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

/// How `ContextWake` asks a coding agent to continue from a handoff, for
/// [`AgentAdapter::launch_with_handoff`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffLaunch {
    /// Directory of the validated AWHF handoff; it contains `context.md`.
    pub handoff_directory: PathBuf,
    /// The complete first message for the agent, built, bounded and redacted
    /// by `ContextWake` with LF line endings. Adapters pass it unchanged as
    /// one argument.
    pub prompt: String,
    /// Unique text contained in `prompt`, used afterwards to confirm from
    /// the agent's transcript that the prompt was delivered.
    pub marker: String,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    /// Natively resume this provider session and deliver `prompt` into it.
    /// `None` starts a new session.
    pub resume_session_id: Option<String>,
    /// A provider session ID for a new session, for agents that accept one
    /// (Claude Code `--session-id`). Ignored when resuming and by agents that
    /// cannot assign session IDs.
    pub new_session_id: Option<String>,
}

/// What `ContextWake` knows about a launch it performed, for
/// [`AgentAdapter::launched_session`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRecord {
    /// When `ContextWake` started the agent process.
    pub started_at: DateTime<Utc>,
    /// The marker contained in the delivered prompt.
    pub marker: String,
    /// The session the launch resumed, or the ID it assigned to a new one.
    pub expected_session_id: Option<String>,
}

/// The provider session a launch created or resumed, as found afterwards in
/// the agent's own local session store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchedSession {
    pub provider_session_id: String,
    pub title: Option<String>,
    pub last_activity_at: Option<DateTime<Utc>>,
    /// A user message containing the launch marker is in the session's
    /// transcript, so the agent received the handoff prompt.
    pub prompt_delivered: bool,
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

    /// The configuration home the agent uses when `ContextWake` sets no
    /// override, which is where the user's existing sign-in lives. `None`
    /// means profiles of this agent always use a `ContextWake`-managed home.
    ///
    /// For a profile that uses this home, `ContextWake` never sets the
    /// agent's home variable, never writes into the directory and never
    /// deletes it.
    fn default_home(&self) -> Option<PathBuf> {
        None
    }

    /// Reads a bounded, redacted excerpt of the agent's conversation in
    /// `workspace` from its local transcripts: the given provider session, or
    /// the most recently active one when `provider_session_id` is `None`.
    /// `Ok(None)` means that no transcript was found for the workspace.
    fn conversation_excerpt(
        &self,
        _agent_home: &Path,
        _workspace: &Path,
        _provider_session_id: Option<&str>,
    ) -> Result<Option<ConversationExcerpt>> {
        Err(ContextWakeError::CapabilityUnavailable(format!(
            "{} {NO_CONVERSATION_EXCERPT}",
            self.display_name()
        )))
    }

    /// Runs the agent interactively in `workspace` with `launch.prompt` as
    /// its first message, and returns when the agent exits. With
    /// `launch.resume_session_id` the agent natively resumes that session and
    /// the prompt is delivered into it; otherwise a new session starts.
    ///
    /// The default implementation starts a new session through
    /// [`Self::start_with_handoff`] (with the adapter's own prompt) and
    /// refuses to resume.
    fn launch_with_handoff(
        &self,
        agent_home: &Path,
        workspace: &Path,
        launch: &HandoffLaunch,
    ) -> Result<()> {
        if launch.resume_session_id.is_some() {
            return Err(ContextWakeError::CapabilityUnavailable(format!(
                "{} {NO_RESUME_WITH_HANDOFF}",
                self.display_name()
            )));
        }
        self.start_with_handoff(
            agent_home,
            workspace,
            &launch.handoff_directory,
            launch.model_provider.as_deref(),
            launch.model.as_deref(),
        )
    }

    /// Finds the provider session that the launch described by `record`
    /// created or resumed in `workspace`, and whether the handoff prompt
    /// reached it. `Ok(None)` means the adapter cannot tell, in which case
    /// `ContextWake` must not claim that the context was delivered.
    fn launched_session(
        &self,
        _agent_home: &Path,
        _workspace: &Path,
        _record: &LaunchRecord,
    ) -> Result<Option<LaunchedSession>> {
        Ok(None)
    }
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

    /// A registry of exactly these adapters, for tests of code that looks
    /// agents up through the registry.
    #[cfg(test)]
    pub(crate) fn from_adapters(adapters: Vec<Arc<dyn AgentAdapter>>) -> Self {
        Self { adapters }
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

    fn launch(resume_session_id: Option<&str>) -> HandoffLaunch {
        HandoffLaunch {
            handoff_directory: PathBuf::from("handoff"),
            prompt: "ContextWake handoff test".into(),
            marker: "ContextWake handoff test".into(),
            model_provider: None,
            model: None,
            resume_session_id: resume_session_id.map(str::to_string),
            new_session_id: None,
        }
    }

    #[test]
    fn continuity_hooks_default_to_honest_refusals() {
        let root = tempfile::tempdir().expect("tempdir");
        let adapter = CursorAdapter::with_executable(root.path().join("missing-agent"));
        assert_eq!(adapter.default_home(), None);
        assert!(matches!(
            adapter.conversation_excerpt(root.path(), root.path(), None),
            Err(ContextWakeError::CapabilityUnavailable(_))
        ));
        let record = LaunchRecord {
            started_at: chrono::Utc::now(),
            marker: "marker".into(),
            expected_session_id: None,
        };
        assert_eq!(
            adapter
                .launched_session(root.path(), root.path(), &record)
                .expect("no verification"),
            None
        );
        // Resuming with a handoff is refused before any process is built.
        let before = commands_built_on_this_thread();
        assert!(matches!(
            adapter.launch_with_handoff(root.path(), root.path(), &launch(Some("session"))),
            Err(ContextWakeError::CapabilityUnavailable(_))
        ));
        assert_eq!(commands_built_on_this_thread(), before);
    }

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
