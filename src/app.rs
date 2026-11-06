use std::cmp::Reverse;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use clap::CommandFactory;
use serde::Serialize;
use uuid::Uuid;

use crate::checkpoint::{CheckpointInput, CheckpointService, LatestCheckpoint};
use crate::cli::{
    AgentCommand, CheckpointCommand, Cli, Command, ConfigCommand, HandoffCommand, ModelCommand,
    ProfileCommand, SessionCommand, WorkspaceCommand,
};
use crate::config::AppConfig;
use crate::continuity::{
    ContinueNotice, ContinueOutcome, ContinueRequest, SwitchOutcome, SwitchRequest,
    continue_with_handoff, recorded_session, switch_profile_with,
};
use crate::doctor::{CheckStatus, DoctorReport, run_doctor, run_doctor_without_state};
use crate::error::{ContextWakeError, IoContext, Result};
use crate::git::GitClient;
use crate::guardian::assess;
use crate::handoff::HandoffService;
use crate::model::{
    AgentHealth, AuthState, CapabilitySupport, Checkpoint, ContinuityKind, ConversationExcerpt,
    Profile, ProfileHomeMode, ResumeCapability, Session, TrustState, Workspace,
};
use crate::paths::{
    AppPaths, canonicalize_simplified, path_is_within, paths_equivalent, simplify_path,
};
use crate::provider::AgentRegistry;
use crate::security::{
    SecretScanner, profile_slug, sanitize_terminal, sanitize_untrusted_label,
    validate_external_reference, validate_local_name,
};
use crate::store::Store;
use crate::validation::ValidationRunner;
use crate::workspace::{
    canonical_workspace_directory, detect_workspace, load_project_config,
    read_project_instructions, read_project_metadata,
};
use crate::{BINARY_NAME, PRODUCT_NAME, VERSION};

/// How many sessions `ctx session sync` asks an agent for. `--max-count` is
/// applied after the workspace filter, so the listing must not be cut to it
/// first: agents that list every workspace would otherwise crowd it out.
const SESSION_DISCOVERY_LIMIT: usize = 500;

#[derive(Clone, Debug)]
pub struct Application {
    pub paths: AppPaths,
    pub config: AppConfig,
    pub store: Store,
    pub git: GitClient,
    pub agents: AgentRegistry,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusView {
    pub application: String,
    pub version: String,
    pub active_profile: Option<Profile>,
    pub workspace: Workspace,
    pub git: Option<crate::model::GitSnapshot>,
    pub agent: crate::model::AgentHealth,
    pub latest_session: Option<Session>,
    pub context_information: String,
    pub continuity_guardian: crate::model::ContinuityRiskStatus,
}

/// A status view plus the problems that were degraded to warnings instead of
/// failing the command.
struct StatusReport {
    view: StatusView,
    warnings: Vec<String>,
    git_unavailable: bool,
}

const STATUS_CONTEXT_INFORMATION: &str =
    "Unavailable from stable provider interfaces; local continuity remains available";

/// What `ctx status` shows.
enum StatusOutcome {
    Report(Box<StatusReport>),
    /// Run from the home directory or a root with no usable active workspace:
    /// there is no workspace to show, and the first warning says how to pick
    /// one.
    NoWorkspace(Box<WorkspacelessStatus>),
}

struct WorkspacelessStatus {
    active_profile: Option<Profile>,
    agent: AgentHealth,
    warnings: Vec<String>,
}

/// Which configuration home a new profile uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileHomeChoice {
    /// The agent's default home (the existing sign-in) when the agent has
    /// one and no other profile uses it; otherwise a managed home.
    #[default]
    Automatic,
    /// A `ContextWake`-managed home with a separate sign-in.
    Isolated,
    /// The agent's default home; an error when that is not possible.
    ExistingLogin,
}

/// What [`Application::add_profile`] creates.
#[derive(Clone, Copy, Debug)]
pub struct NewProfile<'a> {
    pub name: &'a str,
    pub display_name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub agent_id: &'a str,
    pub model_provider_id: Option<&'a str>,
    pub model_preference: Option<&'a str>,
    /// See [`Application::create_profile_with_options`].
    pub allow_shared_identity: bool,
    pub home: ProfileHomeChoice,
}

/// A created profile and whether the agent's existing sign-in was available
/// when its home was chosen.
#[derive(Clone, Debug)]
pub struct CreatedProfile {
    pub profile: Profile,
    pub existing_login: ExistingLogin,
}

/// Whether a new profile can use an agent's own default home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExistingLogin {
    Available(PathBuf),
    /// Another profile already uses the default home (agent homes are
    /// unique, so at most one profile uses the existing sign-in).
    InUse {
        home: PathBuf,
        profile: String,
    },
    /// The agent's configured home is a `ContextWake`-managed home, for
    /// example because `ctx` runs inside an agent that `ContextWake`
    /// launched for another profile.
    Managed(PathBuf),
    /// The adapter has no default home to share.
    Unsupported,
}

impl ExistingLogin {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// Why the existing sign-in cannot be used, for `agent` (a display name).
    pub fn reason(&self, agent: &str) -> String {
        match self {
            Self::Available(home) => {
                format!(
                    "the existing {agent} sign-in in {} is available",
                    display_path(home)
                )
            }
            Self::InUse { home, profile } => format!(
                "profile {} already uses the existing {agent} sign-in in {}",
                terminal_line(profile),
                display_path(home)
            ),
            Self::Managed(home) => format!(
                "the {agent} home in effect ({}) is a ContextWake-managed profile home, not your own {agent} sign-in",
                display_path(home)
            ),
            Self::Unsupported => {
                format!("ContextWake cannot use an existing {agent} sign-in for profiles")
            }
        }
    }
}

/// Decides [`ExistingLogin`] from the adapter's default home, the managed
/// agent-homes directory and the existing profiles.
pub fn existing_login_status(
    default_home: Option<&Path>,
    managed_homes: &Path,
    profiles: &[Profile],
) -> ExistingLogin {
    let Some(home) = default_home.map(simplify_path) else {
        return ExistingLogin::Unsupported;
    };
    if path_is_within(&home, managed_homes) {
        return ExistingLogin::Managed(home);
    }
    if let Some(profile) = profiles
        .iter()
        .find(|profile| paths_equivalent(&profile.agent_home, &home))
    {
        return ExistingLogin::InUse {
            home,
            profile: profile.display_name.clone(),
        };
    }
    ExistingLogin::Available(home)
}

/// One line that says which sign-in a profile uses.
pub fn profile_home_summary(profile: &Profile, agent: &str) -> String {
    match profile.home_mode {
        ProfileHomeMode::AgentDefault => format!(
            "existing {agent} sign-in (the agent's own home {}; ContextWake never changes it)",
            display_path(&profile.agent_home)
        ),
        ProfileHomeMode::Isolated => format!(
            "separate ContextWake sign-in (managed home {})",
            display_path(&profile.agent_home)
        ),
    }
}

/// What `ctx profile add` prints: the chosen mode and what it means.
/// `explicitly_isolated` is true when the user asked for a managed home.
pub fn profile_created_message(
    created: &CreatedProfile,
    agent: &str,
    explicitly_isolated: bool,
) -> String {
    let profile = &created.profile;
    let mut message = format!(
        "Created profile {} ({}).\nSign-in: {}.",
        terminal_line(&profile.display_name),
        profile.agent_id,
        profile_home_summary(profile, agent)
    );
    match profile.home_mode {
        ProfileHomeMode::AgentDefault => {
            let _ = write!(
                message,
                "\nThis profile uses the sign-in you already use with {agent} outside ContextWake. ContextWake launches {agent} without overriding its home and never writes into or deletes that home.\nAuth: {}.",
                profile.auth_state.as_str()
            );
            if profile.auth_state != AuthState::SignedIn {
                let _ = write!(
                    message,
                    "\nSign in to {agent} the way you normally do, or run:\n  ctx profile login {}\n(this signs in your normal {agent} login, not a separate one)",
                    profile.name
                );
            }
        }
        ProfileHomeMode::Isolated => {
            if !explicitly_isolated && !created.existing_login.is_available() {
                let _ = write!(
                    message,
                    "\nThe existing sign-in was not used: {}.",
                    created.existing_login.reason(agent)
                );
            }
            let _ = write!(
                message,
                "\nThis profile signs in separately from your normal {agent} login. Credentials remain provider-owned. Sign in with:\n  ctx profile login {}",
                profile.name
            );
        }
    }
    message
}

/// For a profile that uses the agent's own default home: a notice that
/// signing `direction` ("in" or "out") changes the user's normal agent login.
fn existing_login_notice(profile: &Profile, agent: &str, direction: &str) -> Option<String> {
    (profile.home_mode == ProfileHomeMode::AgentDefault).then(|| {
        format!(
            "Profile {} uses your existing {agent} sign-in ({}). Signing {direction} here signs your normal {agent} login {direction}, also outside ContextWake.",
            terminal_line(&profile.display_name),
            display_path(&profile.agent_home)
        )
    })
}

/// Short label for profile lists.
const fn home_mode_tag(mode: ProfileHomeMode) -> &'static str {
    match mode {
        ProfileHomeMode::AgentDefault => "existing-login",
        ProfileHomeMode::Isolated => "isolated",
    }
}

/// The workspace an omitted `--workspace` refers to.
enum DirectoryTarget {
    /// Rules 1, 2 and 4, with the warning raised while reading trusted project
    /// metadata, if any.
    Workspace(Box<Workspace>, Option<String>),
    /// Rule 3: `ContextWake` never picks a workspace implicitly from here.
    Refused { directory: PathBuf, reason: String },
}

impl Application {
    pub fn discover() -> Result<Self> {
        let paths = AppPaths::discover()?;
        paths.ensure()?;
        let config = AppConfig::read_or_create(&paths)?;
        let store = Store::open(paths.state_db())?;
        let git = GitClient::new(Duration::from_millis(config.git_timeout_ms));
        let agents = AgentRegistry::discover();
        Ok(Self {
            paths,
            config,
            store,
            git,
            agents,
        })
    }

    pub fn run(&self, cli: Cli) -> Result<i32> {
        match cli.command {
            None => {
                crate::tui::run(self.clone(), cli.ascii || self.config.ascii)?;
                Ok(0)
            }
            Some(Command::Status(args)) => {
                match self.status_report_for(args.path.as_deref())? {
                    StatusOutcome::Report(report) => print_status(&report, cli.json, cli.ascii),
                    StatusOutcome::NoWorkspace(status) => {
                        print_workspaceless_status(&status, cli.json);
                    }
                }
                Ok(0)
            }
            Some(Command::Doctor(args)) => {
                let report = run_doctor(
                    &self.paths,
                    &self.config,
                    &self.store,
                    &self.git,
                    &self.agents,
                    args.verbose,
                );
                print_doctor(&report, cli.json, cli.ascii);
                Ok(i32::from(report.has_failures()))
            }
            Some(Command::Profile(args)) => {
                self.profile_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Workspace(args)) => self.workspace_command(args.command, cli.json),
            Some(Command::Session(args)) => {
                self.session_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Checkpoint(args)) => {
                self.checkpoint_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Handoff(args)) => {
                self.handoff_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Usage) => {
                let usage = self.store.usage_summary()?;
                if cli.json {
                    print_json(&usage);
                } else {
                    println!("PROVIDER-REPORTED USAGE");
                    println!("  {}\n", usage.provider_usage_message);
                    println!("LOCAL ACTIVITY");
                    println!("  Sessions         {}", usage.local_session_count);
                    println!("  Profile switches {}", usage.local_profile_switch_count);
                    println!("  Checkpoints      {}", usage.local_checkpoint_count);
                    println!("  Handoffs         {}", usage.local_handoff_count);
                }
                Ok(0)
            }
            Some(Command::Agent(args)) => {
                self.agent_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Model(args)) => {
                self.model_command(args.command, cli.json)?;
                Ok(0)
            }
            Some(Command::Config(args)) => {
                config_command(&self.paths, Some(&self.config), args.command, cli.json)
            }
            Some(Command::Completion(args)) => print_completion(args.shell, BINARY_NAME),
            Some(Command::Version) => print_version(BINARY_NAME, cli.json),
        }
    }

    pub fn status(&self, path: &Path) -> Result<StatusView> {
        let (agent, _) = self.active_agent_health()?;
        self.status_with_agent(path, agent)
    }

    pub(crate) fn status_with_agent(
        &self,
        path: &Path,
        agent: crate::model::AgentHealth,
    ) -> Result<StatusView> {
        let (workspace, warning) =
            self.register_workspace_reporting(path, TrustState::Untrusted, false)?;
        self.status_report(workspace, agent, warning.into_iter().collect())
            .map(|report| report.view)
    }

    /// `ctx status [PATH]`. Without a path the workspace follows the rules of an
    /// omitted `--workspace`. Where those refuse (the home directory or a drive
    /// root), the active workspace is shown with a warning instead, because
    /// status changes nothing. Without a usable active workspace (a fresh
    /// install, or one whose directory was removed) the status has no
    /// workspace and carries the guidance as a warning, so `ctx status` stays
    /// usable from there.
    fn status_report_for(&self, path: Option<&Path>) -> Result<StatusOutcome> {
        let (agent, mut warnings) = self.active_agent_health()?;
        let workspace = if let Some(path) = path {
            let (workspace, warning) =
                self.register_workspace_reporting(path, TrustState::Untrusted, false)?;
            warnings.extend(warning);
            workspace
        } else {
            match self.directory_target(&current_directory()?, &home_directories())? {
                DirectoryTarget::Workspace(workspace, warning) => {
                    warnings.extend(warning);
                    *workspace
                }
                DirectoryTarget::Refused { directory, reason } => {
                    let (workspace, warning) =
                        match self.active_workspace_instead(&directory, &reason, "ctx status") {
                            Ok(found) => found,
                            Err(refusal) => {
                                let message = match refusal {
                                    ContextWakeError::WorkspaceNotFound(message) => message,
                                    other => safe_message(&other),
                                };
                                warnings.insert(0, message);
                                return Ok(StatusOutcome::NoWorkspace(Box::new(
                                    WorkspacelessStatus {
                                        active_profile: self.store.active_profile()?,
                                        agent,
                                        warnings,
                                    },
                                )));
                            }
                        };
                    warnings.insert(
                        0,
                        format!(
                            "the current directory {} is {reason}, so the active workspace is shown. Commands run from here need --workspace <path>.",
                            display_path(&directory)
                        ),
                    );
                    warnings.extend(warning);
                    workspace
                }
            }
        };
        self.status_report(workspace, agent, warnings)
            .map(|report| StatusOutcome::Report(Box::new(report)))
    }

    /// The active workspace, re-registered, for the interactive fallback of
    /// rule 3; without a usable one, the rule-3 refusal.
    fn active_workspace_instead(
        &self,
        directory: &Path,
        reason: &str,
        explicit: &str,
    ) -> Result<(Workspace, Option<String>)> {
        let refusal = || self.workspace_refusal(directory, reason, explicit);
        let active = self
            .store
            .active_workspace()
            .ok()
            .flatten()
            .ok_or_else(refusal)?;
        self.register_workspace_reporting(&active.path, TrustState::Untrusted, false)
            .map_err(|_| refusal())
    }

    /// The active profile's agent health. A failed probe degrades to an
    /// unavailable agent plus a warning instead of failing `ctx status`.
    fn active_agent_health(&self) -> Result<(AgentHealth, Vec<String>)> {
        let Some(profile) = self.store.active_profile()? else {
            return Ok((unconfigured_agent_health(), Vec::new()));
        };
        let detected = self
            .agents
            .get(&profile.agent_id)
            .and_then(|adapter| adapter.detect(Some(&profile.agent_home)));
        Ok(match detected {
            Ok(health) => (health, Vec::new()),
            Err(error) => {
                let message = safe_message(&error);
                let warning = format!("could not check agent {}: {message}", profile.agent_id);
                (
                    AgentHealth {
                        agent_id: profile.agent_id.clone(),
                        installed: false,
                        executable: None,
                        version: None,
                        auth_state: AuthState::Unknown,
                        message,
                    },
                    vec![warning],
                )
            }
        })
    }

    fn status_report(
        &self,
        workspace: Workspace,
        agent: AgentHealth,
        mut warnings: Vec<String>,
    ) -> Result<StatusReport> {
        let mut active_profile = self.store.active_profile()?;
        if let Some(profile) = active_profile.as_mut() {
            let previous_auth_state = profile.auth_state;
            profile.auth_state = agent.auth_state;
            if agent.installed && previous_auth_state != agent.auth_state {
                self.store
                    .update_profile_auth(profile.id, agent.auth_state)?;
            }
        }
        // Without a usable Git executable the workspace is an ordinary
        // directory (snapshot() returns no state); say so instead of showing
        // a workspace without Git state as if it had none.
        let mut git_unavailable = false;
        if let Some(reason) = self.git.unavailable_reason() {
            git_unavailable = true;
            warnings.push(format!(
                "Git state is unavailable: {}",
                terminal_line(&reason)
            ));
        }
        let (git, changed_files) = match self.git.snapshot(&workspace.path) {
            Ok(git) => {
                let changed_files =
                    self.git
                        .changed_files(&workspace.path)
                        .unwrap_or_else(|error| {
                            warnings.push(format!(
                                "changed files are unavailable: {}",
                                safe_message(&error)
                            ));
                            Vec::new()
                        });
                (git, changed_files)
            }
            Err(error) => {
                git_unavailable = true;
                warnings.push(format!(
                    "Git state is unavailable: {}",
                    safe_message(&error)
                ));
                (None, Vec::new())
            }
        };
        let latest = self
            .checkpoints()
            .latest_for_workspace_with_unreadable(workspace.id)
            .unwrap_or_else(|error| {
                warnings.push(format!(
                    "checkpoints could not be read ({}). 'ctx checkpoint list' shows them, and 'ctx checkpoint delete <id> --yes' removes an unreadable one.",
                    safe_message(&error)
                ));
                LatestCheckpoint::default()
            });
        if !latest.unreadable.is_empty() {
            // A newer checkpoint that cannot be read must not silently make an
            // older one look like the latest saved state.
            let ids = latest
                .unreadable
                .iter()
                .map(|entry| entry.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "checkpoints could not be read ({ids}); status uses the newest readable checkpoint. 'ctx checkpoint list' shows them, and 'ctx checkpoint delete <id> --yes' removes an unreadable one."
            ));
        }
        let latest_checkpoint = latest.checkpoint;
        let continuity_guardian = assess(
            latest_checkpoint.as_ref(),
            git.as_ref(),
            &changed_files,
            Utc::now(),
        );
        let latest_session = self
            .store
            .list_sessions(false)?
            .into_iter()
            .find(|session| session.workspace_id == workspace.id);
        Ok(StatusReport {
            view: StatusView {
                application: PRODUCT_NAME.into(),
                version: VERSION.into(),
                active_profile,
                workspace,
                git,
                agent,
                latest_session,
                context_information: STATUS_CONTEXT_INFORMATION.into(),
                continuity_guardian,
            },
            warnings,
            git_unavailable,
        })
    }

    pub fn register_workspace(
        &self,
        path: &Path,
        trust: TrustState,
        activate: bool,
    ) -> Result<Workspace> {
        self.register_workspace_reporting(path, trust, activate)
            .map(|(workspace, _)| workspace)
    }

    /// Registers `path` like [`Self::register_workspace`] and also returns the
    /// warning raised while reading its trusted project metadata. An invalid
    /// `project.toml` is ignored with a warning here; explicit validation still
    /// reports it as an error.
    fn register_workspace_reporting(
        &self,
        path: &Path,
        trust: TrustState,
        activate: bool,
    ) -> Result<(Workspace, Option<String>)> {
        let mut workspace = detect_workspace(path, &self.git, trust)?;
        let mut stored_path = None;
        if let Some(existing) = self.existing_workspace_row(&workspace.path)? {
            workspace.id = existing.id;
            workspace.created_at = existing.created_at;
            if existing.trust_state == TrustState::Trusted {
                workspace.trust_state = TrustState::Trusted;
            }
            if existing.path != workspace.path {
                stored_path = Some(existing.path);
            }
        }
        let metadata = read_project_metadata(&workspace);
        if let Some(project) = metadata.config {
            if let Some(name) = project.name {
                workspace.display_name = name;
            }
            workspace.preferred_agent_id = project.preferred_agent;
            workspace.preferred_model_provider_id = project.preferred_model_provider;
            workspace.preferred_model = project.preferred_model;
        }
        match stored_path {
            // Update a row stored under another spelling of the same directory
            // (for example the Windows verbatim form written by older builds)
            // instead of inserting a duplicate workspace.
            Some(stored_path) => self.store.upsert_workspace(&Workspace {
                path: stored_path,
                ..workspace.clone()
            })?,
            None => self.store.upsert_workspace(&workspace)?,
        }
        if activate {
            self.store.set_active_workspace(workspace.id)?;
        }
        Ok((workspace, metadata.warning))
    }

    fn existing_workspace_row(&self, path: &Path) -> Result<Option<Workspace>> {
        if let Some(existing) = self.store.workspace_by_path(path)? {
            return Ok(Some(existing));
        }
        Ok(self
            .store
            .list_workspaces()?
            .into_iter()
            .find(|candidate| paths_equivalent(&candidate.path, path)))
    }

    /// The workspace a CLI command operates on. It is registered (untrusted
    /// unless already trusted) but not activated.
    ///
    /// An explicit `path` always wins: that directory, or the Git work tree that
    /// contains it. Without one, the workspace comes from the current directory
    /// and never from the last active workspace:
    /// 1. the longest registered workspace that contains it;
    /// 2. else the top level of the Git work tree that contains it;
    /// 3. else, in the home directory or at a drive or filesystem root, the
    ///    command is refused with guidance naming the active workspace;
    /// 4. else the current directory itself.
    ///
    /// Registered workspaces and Git work trees that are themselves the home
    /// directory or a root are never chosen implicitly (earlier versions
    /// registered the home directory automatically).
    pub fn workspace_for_command(&self, path: Option<&Path>) -> Result<Workspace> {
        self.command_workspace(path).map(|(workspace, _)| workspace)
    }

    /// [`Self::workspace_for_command`] plus the project-metadata warning.
    fn command_workspace(&self, path: Option<&Path>) -> Result<(Workspace, Option<String>)> {
        if let Some(path) = path {
            return self.register_workspace_reporting(path, TrustState::Untrusted, false);
        }
        match self.directory_target(&current_directory()?, &home_directories())? {
            DirectoryTarget::Workspace(workspace, warning) => Ok((*workspace, warning)),
            DirectoryTarget::Refused { directory, reason } => {
                Err(self.workspace_refusal(&directory, &reason, "--workspace"))
            }
        }
    }

    /// The workspace of the current directory under rules 1, 2 and 4 of
    /// [`Self::workspace_for_command`], or `None` where rule 3 refuses (the home
    /// directory or a root), in which case callers such as the TUI fall back to
    /// the active workspace.
    pub fn current_directory_workspace(&self) -> Result<Option<Workspace>> {
        Ok(
            match self.directory_target(&current_directory()?, &home_directories())? {
                DirectoryTarget::Workspace(workspace, _) => Some(*workspace),
                DirectoryTarget::Refused { .. } => None,
            },
        )
    }

    /// Compatibility wrapper for interactive callers such as the TUI, which
    /// shows the current directory's workspace and otherwise the active one.
    /// It resolves like [`Self::workspace_for_command`], except that where rule
    /// 3 refuses (the home directory or a root) the active workspace, which is
    /// what such callers display, is used. The result becomes the active
    /// workspace. CLI commands use [`Self::workspace_for_command`].
    pub fn resolve_workspace(&self, path: Option<&Path>) -> Result<Workspace> {
        let workspace = if path.is_some() {
            self.workspace_for_command(path)?
        } else {
            self.interactive_workspace(&current_directory()?, &home_directories())?
        };
        self.store.set_active_workspace(workspace.id)?;
        Ok(workspace)
    }

    fn interactive_workspace(&self, directory: &Path, homes: &[PathBuf]) -> Result<Workspace> {
        match self.directory_target(directory, homes)? {
            DirectoryTarget::Workspace(workspace, _) => Ok(*workspace),
            DirectoryTarget::Refused { directory, reason } => self
                .active_workspace_instead(&directory, &reason, "--workspace")
                .map(|(workspace, _)| workspace),
        }
    }

    /// Applies the rules of [`Self::workspace_for_command`] to `directory`
    /// (canonical); `homes` are the home-directory spellings to refuse.
    fn directory_target(&self, directory: &Path, homes: &[PathBuf]) -> Result<DirectoryTarget> {
        let registered = self
            .store
            .list_workspaces()?
            .into_iter()
            .filter(|workspace| {
                broad_root_reason(&workspace.path, homes).is_none()
                    && path_is_within(directory, &workspace.path)
            })
            .max_by_key(|workspace| simplify_path(&workspace.path).as_os_str().len());
        let git_root = self
            .git
            .repository_root(directory)
            .ok()
            .flatten()
            .map(|root| canonicalize_simplified(&root).unwrap_or_else(|_| simplify_path(&root)));
        // The most specific candidate wins: a Git work tree nested inside a
        // registered directory (for example a repository below an ordinary
        // folder where `ctx status` once ran) is its own workspace, while a
        // registered directory inside the work tree, or the work tree itself,
        // keeps its registration.
        let registered = registered.filter(|registered| {
            git_root.as_ref().is_none_or(|git_root| {
                !path_is_within(git_root, &registered.path)
                    || paths_equivalent(git_root, &registered.path)
            })
        });
        let root = if let Some(registered) = registered {
            registered.path
        } else if let Some(root) = git_root {
            if let Some(reason) = broad_root_reason(&root, homes) {
                return Ok(DirectoryTarget::Refused {
                    directory: directory.to_path_buf(),
                    reason: format!(
                        "inside a Git work tree whose top level {} is {reason}",
                        display_path(&root)
                    ),
                });
            }
            root
        } else if let Some(reason) = broad_root_reason(directory, homes) {
            return Ok(DirectoryTarget::Refused {
                directory: directory.to_path_buf(),
                reason: reason.into(),
            });
        } else {
            directory.to_path_buf()
        };
        let (workspace, warning) =
            self.register_workspace_reporting(&root, TrustState::Untrusted, false)?;
        Ok(DirectoryTarget::Workspace(Box::new(workspace), warning))
    }

    /// Guidance for rule 3. `explicit` is how the command names a workspace
    /// explicitly (`--workspace`, or `ctx status` with a path argument).
    fn workspace_refusal(
        &self,
        directory: &Path,
        reason: &str,
        explicit: &str,
    ) -> ContextWakeError {
        let mut message = format!(
            "no workspace was selected because the current directory {} is {reason}; ContextWake never picks a workspace there implicitly. Run the command inside a project directory, or name the workspace explicitly ({explicit} <path>).",
            display_path(directory)
        );
        if let Ok(Some(active)) = self.store.active_workspace() {
            if active.path.is_dir() {
                let _ = write!(
                    message,
                    " The active workspace is {}; to use it, pass {explicit} \"{}\".",
                    workspace_label(&active),
                    display_path(&active.path)
                );
            } else {
                let _ = write!(
                    message,
                    " The active workspace {} no longer exists; remove it with 'ctx workspace remove {} --yes'.",
                    workspace_label(&active),
                    active.id
                );
            }
        }
        ContextWakeError::WorkspaceNotFound(message)
    }

    /// Selects `workspace` as the active workspace once a command operating on
    /// it has succeeded, so a later TUI start from the home directory shows it.
    /// The selection is a convenience: failing to record it must not turn the
    /// completed operation into an error.
    fn select_after_success(&self, workspace: &Workspace) {
        let _ = self.store.set_active_workspace(workspace.id);
    }

    pub fn checkpoints(&self) -> CheckpointService {
        CheckpointService::new(self.store.clone(), self.paths.clone(), self.git.clone())
    }

    pub fn handoffs(&self) -> HandoffService {
        HandoffService::new(self.store.clone(), self.paths.clone())
    }

    fn run_project_validation(
        &self,
        workspace: &Workspace,
    ) -> Result<Vec<crate::model::ValidationResult>> {
        let project = load_project_config(workspace)?.ok_or_else(|| {
            ContextWakeError::Configuration(
                "no .contextwake/project.toml exists for this workspace".into(),
            )
        })?;
        ValidationRunner::new(Duration::from_millis(self.config.validation_timeout_ms))
            .run(workspace, &project.validation)
    }

    pub fn create_profile(
        &self,
        name: &str,
        display_name: Option<&str>,
        description: Option<&str>,
    ) -> Result<Profile> {
        self.create_profile_for_agent(name, display_name, description, "codex", None, None)
    }

    pub fn create_profile_for_agent(
        &self,
        name: &str,
        display_name: Option<&str>,
        description: Option<&str>,
        agent_id: &str,
        model_provider_id: Option<&str>,
        model_preference: Option<&str>,
    ) -> Result<Profile> {
        self.create_profile_with_options(
            name,
            display_name,
            description,
            agent_id,
            model_provider_id,
            model_preference,
            false,
        )
    }

    /// Like [`Self::create_profile_for_agent`]. Another profile for an agent
    /// that cannot keep separate sign-ins per profile (its `multiple_profiles`
    /// capability is unsupported, as for Kiro CLI and Cursor CLI) is refused
    /// unless `allow_shared_identity` acknowledges that the profiles share one
    /// provider identity.
    ///
    /// The profile always gets a `ContextWake`-managed home; see
    /// [`Self::add_profile`] for profiles that use the existing sign-in.
    pub fn create_profile_with_options(
        &self,
        name: &str,
        display_name: Option<&str>,
        description: Option<&str>,
        agent_id: &str,
        model_provider_id: Option<&str>,
        model_preference: Option<&str>,
        allow_shared_identity: bool,
    ) -> Result<Profile> {
        self.add_profile(&NewProfile {
            name,
            display_name,
            description,
            agent_id,
            model_provider_id,
            model_preference,
            allow_shared_identity,
            home: ProfileHomeChoice::Isolated,
        })
        .map(|created| created.profile)
    }

    /// Whether a new profile for `agent_id` can use the agent's own default
    /// home (the user's existing sign-in).
    pub fn existing_login(&self, agent_id: &str) -> Result<ExistingLogin> {
        let adapter = self.agents.get(agent_id)?;
        Ok(existing_login_status(
            adapter.default_home().as_deref(),
            &self.paths.agent_homes_dir(),
            &self.store.list_profiles()?,
        ))
    }

    /// Creates a profile. `request.home` decides whether it uses the agent's
    /// own default home (the existing sign-in: `ContextWake` never sets the
    /// agent's home variable, never initializes, writes into or deletes that
    /// directory) or a `ContextWake`-managed home that signs in separately.
    pub fn add_profile(&self, request: &NewProfile<'_>) -> Result<CreatedProfile> {
        let requested_name = validate_local_name(request.name)?;
        let name = profile_slug(&requested_name)?;
        let display_name = validate_local_name(request.display_name.unwrap_or(&requested_name))?;
        let description = request.description;
        let allow_shared_identity = request.allow_shared_identity;
        let requested_agent = validate_external_reference(request.agent_id)?.to_ascii_lowercase();
        let adapter = self.agents.get(&requested_agent)?;
        let agent_id = adapter.id().to_string();
        let mut model_provider_id = request
            .model_provider_id
            .map(validate_external_reference)
            .transpose()?
            .map(|value| value.to_ascii_lowercase());
        if let Some(provider) = &model_provider_id
            && !adapter.accepts_model_provider(provider)
        {
            return Err(ContextWakeError::CapabilityUnavailable(format!(
                "model provider {provider} is not declared by agent {agent_id}"
            )));
        }
        let mut model_preference = request
            .model_preference
            .map(validate_external_reference)
            .transpose()?;
        if model_preference.is_some() && !adapter.capabilities().model_selection.is_available() {
            return Err(ContextWakeError::CapabilityUnavailable(format!(
                "agent {agent_id} does not support model selection"
            )));
        }
        if let Some(model) = &model_preference {
            let normalized =
                adapter.normalize_model_selection(model_provider_id.as_deref(), model)?;
            model_provider_id = normalized.0;
            model_preference = Some(normalized.1);
        }
        let isolation = adapter.capabilities().multiple_profiles;
        if isolation.support == CapabilitySupport::Unsupported && !allow_shared_identity {
            let existing = self
                .store
                .list_profiles()?
                .into_iter()
                .filter(|profile| profile.agent_id == agent_id)
                .map(|profile| profile.display_name)
                .collect::<Vec<_>>();
            if !existing.is_empty() {
                return Err(ContextWakeError::CapabilityUnavailable(format!(
                    "{} cannot keep separate sign-ins for ContextWake profiles ({}), and profile {} already uses it, so another profile would share that identity. Use the existing profile, or pass --allow-shared-identity to create one that shares it.",
                    adapter.display_name(),
                    isolation.detail,
                    existing.join(", ")
                )));
            }
        }
        let existing_login = existing_login_status(
            adapter.default_home().as_deref(),
            &self.paths.agent_homes_dir(),
            &self.store.list_profiles()?,
        );
        let default_home = match (request.home, &existing_login) {
            (ProfileHomeChoice::Isolated, _) => None,
            (_, ExistingLogin::Available(home)) => Some(home.clone()),
            (ProfileHomeChoice::Automatic, _) => None,
            (ProfileHomeChoice::ExistingLogin, unavailable) => {
                return Err(ContextWakeError::CapabilityUnavailable(format!(
                    "{}. Create the profile with --isolated to give it a separate ContextWake sign-in.",
                    unavailable.reason(adapter.display_name())
                )));
            }
        };
        let id = Uuid::new_v4();
        let (agent_home, home_mode, auth_state) = if let Some(home) = default_home {
            // The sign-in state is probed only after the insert succeeds, so a
            // request that fails (for example on a taken name) never runs the
            // agent's status command against the user's normal sign-in.
            (home, ProfileHomeMode::AgentDefault, AuthState::Unknown)
        } else {
            let agent_home = self.paths.agent_home(&agent_id, &id);
            adapter.initialize_profile_home(&agent_home)?;
            (agent_home, ProfileHomeMode::Isolated, AuthState::SignedOut)
        };
        let now = Utc::now();
        let mut profile = Profile {
            id,
            name,
            display_name,
            agent_id,
            model_provider_id,
            model_preference,
            description: description.map(sanitize_terminal),
            agent_home,
            home_mode,
            account_fingerprint: None,
            auth_state,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        if let Err(error) = self.store.insert_profile(&profile) {
            if profile.home_mode == ProfileHomeMode::Isolated {
                // This UUID-scoped home was created by this operation and has
                // not been exposed to an interactive agent process yet. The
                // agent's own default home is never deleted.
                let _ = std::fs::remove_dir_all(&profile.agent_home);
            }
            return Err(error);
        }
        if profile.home_mode == ProfileHomeMode::AgentDefault {
            // The agent's own home is only read (through the adapter's status
            // probe); a probe failure leaves the state unknown.
            let auth_state = adapter
                .auth_status(&profile.agent_home)
                .unwrap_or(AuthState::Unknown);
            if auth_state != AuthState::Unknown {
                self.store.update_profile_auth(profile.id, auth_state)?;
                profile.auth_state = auth_state;
            }
        }
        let became_active = self.store.active_profile()?.is_none();
        let profile = if became_active {
            self.store.set_active_profile(profile.id)?;
            self.store.profile(&profile.id.to_string())?
        } else {
            profile
        };
        Ok(CreatedProfile {
            profile,
            existing_login,
        })
    }

    /// The agent's display name, or its ID when no adapter implements it.
    fn agent_display_name<'a>(&'a self, agent_id: &'a str) -> &'a str {
        self.agents
            .get(agent_id)
            .map_or(agent_id, |adapter| adapter.display_name())
    }

    /// A warning for agents whose profiles share one sign-in, once more than
    /// one profile uses such an agent.
    fn shared_identity_warning(&self, agent_id: &str) -> Result<Option<String>> {
        let adapter = self.agents.get(agent_id)?;
        let isolation = adapter.capabilities().multiple_profiles;
        if isolation.support != CapabilitySupport::Unsupported {
            return Ok(None);
        }
        let profiles = self
            .store
            .list_profiles()?
            .into_iter()
            .filter(|profile| profile.agent_id == adapter.id())
            .count();
        Ok((profiles > 1).then(|| {
            format!(
                "{name} profiles share one sign-in on this system ({detail}); all {profiles} {name} profiles act as whichever account signed in last.",
                name = adapter.display_name(),
                detail = isolation.detail
            )
        }))
    }

    /// Switches the active profile. With `create_handoff`, the switch
    /// checkpoint also stores the recent conversation of the currently active
    /// profile's agent in the workspace (unless disabled in the
    /// configuration); a transcript that cannot be read is mentioned in the
    /// outcome message and never blocks the switch.
    pub fn switch_active_profile(
        &self,
        target_reference: &str,
        workspace: Option<&Workspace>,
        create_handoff: bool,
        objective: Option<String>,
    ) -> Result<SwitchOutcome> {
        let target = self.validated_switch_target(target_reference)?;
        let (mut outcome, warnings) = self.switch_to_validated_with(
            &target,
            workspace,
            SwitchRequest {
                create_handoff,
                objective,
                ..SwitchRequest::default()
            },
            true,
        )?;
        for warning in warnings {
            outcome.message.push(' ');
            outcome.message.push_str(&warning);
        }
        Ok(outcome)
    }

    /// The recent conversation of `profile`'s agent in `workspace`, for a new
    /// checkpoint, plus a warning when a transcript could not be read. Nothing
    /// is read when `capture` is false or the configuration switches excerpts
    /// off, and agents without readable transcripts are skipped silently.
    pub fn conversation_for_checkpoint(
        &self,
        profile: Option<&Profile>,
        workspace: &Workspace,
        capture: bool,
    ) -> (Option<ConversationExcerpt>, Option<String>) {
        let Some(profile) =
            profile.filter(|_| capture && self.config.capture_conversation_excerpts)
        else {
            return (None, None);
        };
        let Ok(adapter) = self.agents.get(&profile.agent_id) else {
            return (None, None);
        };
        // The agent's newest session in the workspace, and the session that
        // ContextWake launched or resumed for this profile there. An agent's
        // own lookup can leave the latter out (Codex skips non-interactive
        // `codex exec` sessions, for example), and the user may have worked
        // in a newer native session since; the most recently active one wins.
        let newest = adapter.conversation_excerpt(&profile.agent_home, &workspace.path, None);
        let recorded = recorded_session(&self.store, profile, workspace).map(|session| {
            adapter.conversation_excerpt(
                &profile.agent_home,
                &workspace.path,
                Some(&session.provider_session_id),
            )
        });
        match most_recent_excerpt(newest, recorded) {
            Ok(excerpt) => (excerpt, None),
            Err(ContextWakeError::CapabilityUnavailable(_)) => (None, None),
            Err(error) => (
                None,
                Some(format!(
                    "The recent {} conversation could not be read, so the checkpoint has no conversation excerpt: {}",
                    adapter.display_name(),
                    safe_message(&error)
                )),
            ),
        }
    }

    /// Looks up the target profile and confirms that its agent is installed,
    /// before anything is changed.
    fn validated_switch_target(&self, target_reference: &str) -> Result<Profile> {
        let target = self.store.profile(target_reference)?;
        let adapter = self.agents.get(&target.agent_id)?;
        let health = adapter.detect(Some(&target.agent_home))?;
        if !health.installed {
            let reason = health.message.trim().trim_end_matches('.');
            return Err(ContextWakeError::CapabilityUnavailable(format!(
                "{} is unavailable: {reason}. The active profile was not changed.",
                adapter.display_name(),
            )));
        }
        Ok(target)
    }

    /// Switches to a validated target. With a handoff and `capture`, the
    /// source (currently active) profile's recent conversation is captured
    /// first; the returned warnings explain a transcript that could not be
    /// read.
    fn switch_to_validated_with(
        &self,
        target: &Profile,
        workspace: Option<&Workspace>,
        mut request: SwitchRequest,
        capture: bool,
    ) -> Result<(SwitchOutcome, Vec<String>)> {
        let mut warnings = Vec::new();
        if request.create_handoff
            && let Some(workspace) = workspace
        {
            ensure_workspace_directory(workspace)?;
            let source = self.store.active_profile()?;
            let would_switch = source.as_ref().is_none_or(|source| source.id != target.id);
            if would_switch || request.refresh_when_active {
                let (conversation, warning) =
                    self.conversation_for_checkpoint(source.as_ref(), workspace, capture);
                request.conversation = conversation;
                warnings.extend(warning);
            }
        }
        let outcome = switch_profile_with(
            &self.store,
            &self.checkpoints(),
            &self.handoffs(),
            &target.id.to_string(),
            workspace,
            request,
        )?;
        Ok((outcome, warnings))
    }

    /// Continues from a handoff in the active profile's agent, resuming the
    /// profile's own session in the workspace when possible (see
    /// [`Self::continue_handoff_in`]).
    pub fn continue_handoff(&self, handoff: &str, workspace_path: Option<&Path>) -> Result<()> {
        self.continue_handoff_in(handoff, workspace_path, false)
            .map(|_| ())
    }

    /// The continue flow: validates the handoff, decides between resuming
    /// the active profile's own session in the handoff's workspace and a new
    /// session ([`plan_launch`]; `new_session` forces a new one), prints the
    /// decision, launches the agent with `ContextWake`'s bounded launch prompt,
    /// and afterwards records only what the agent's transcript confirms
    /// ([`record_launch`]).
    pub fn continue_handoff_in(
        &self,
        handoff: &str,
        workspace_path: Option<&Path>,
        new_session: bool,
    ) -> Result<ContinueOutcome> {
        self.continue_handoff_reporting(handoff, workspace_path, new_session, false)
    }

    /// [`Self::continue_handoff_in`]; with `progress_to_stderr` the progress
    /// lines go to standard error so that standard output carries only JSON.
    fn continue_handoff_reporting(
        &self,
        handoff: &str,
        workspace_path: Option<&Path>,
        new_session: bool,
        progress_to_stderr: bool,
    ) -> Result<ContinueOutcome> {
        let service = self.handoffs();
        let record = self.store.handoff(handoff)?;
        let (workspace, checkpoint, chosen_explicitly) =
            self.continue_target(handoff, &record, workspace_path)?;
        ensure_workspace_directory(&workspace)?;
        let profile = self.store.active_profile()?.ok_or_else(|| {
            ContextWakeError::InvalidData(
                "an active profile is required; run 'ctx profile use <name>'".into(),
            )
        })?;
        // Validate the manifest and every referenced content digest immediately before launch.
        let verified = service.verify(handoff)?;
        let adapter = self.agents.get(&profile.agent_id)?;
        let outcome = continue_with_handoff(
            &ContinueRequest {
                store: &self.store,
                handoffs: &service,
                adapter,
                profile: &profile,
                workspace: &workspace,
                workspace_label: &workspace_label(&workspace),
                handoff: &verified,
                checkpoint: checkpoint.as_ref(),
                new_session,
            },
            &mut |notice| match notice {
                ContinueNotice::Progress(line) if progress_to_stderr => eprintln!("{line}"),
                ContinueNotice::Progress(line) => println!("{line}"),
                ContinueNotice::Warning(line) => warn(&line),
            },
        )?;
        if chosen_explicitly {
            self.select_after_success(&workspace);
        }
        Ok(outcome)
    }

    /// The workspace a handoff continues in (that of its checkpoint, or
    /// `--workspace` for an imported handoff), its checkpoint, and whether the
    /// workspace was chosen explicitly.
    fn continue_target(
        &self,
        handoff: &str,
        record: &crate::model::HandoffRecord,
        workspace_path: Option<&Path>,
    ) -> Result<(Workspace, Option<Checkpoint>, bool)> {
        let checkpoints = self.checkpoints();
        match checkpoints.read(&record.checkpoint_id.to_string()) {
            Ok(checkpoint) => {
                let workspace = with_conventional_path(
                    self.store.workspace(&checkpoint.workspace_id.to_string())?,
                );
                if let Some(path) = workspace_path {
                    let requested = canonical_workspace_directory(path)?;
                    if !path_is_within(&requested, &workspace.path) {
                        return Err(ContextWakeError::InvalidData(format!(
                            "handoff {handoff} belongs to workspace {} and continues there; omit --workspace or pass that workspace",
                            workspace_label(&workspace)
                        )));
                    }
                }
                Ok((workspace, Some(checkpoint), false))
            }
            Err(ContextWakeError::CheckpointNotFound(_)) if record.checkpoint_id.is_nil() => {
                let path = workspace_path.ok_or_else(|| {
                    ContextWakeError::InvalidData(
                        "this imported handoff has no local checkpoint; pass --workspace".into(),
                    )
                })?;
                Ok((self.workspace_for_command(Some(path))?, None, true))
            }
            Err(ContextWakeError::CheckpointNotFound(_)) => {
                let path = workspace_path.ok_or_else(|| {
                    ContextWakeError::InvalidData(format!(
                        "the checkpoint of handoff {handoff} is missing, so its workspace is unknown; pass --workspace"
                    ))
                })?;
                Ok((self.workspace_for_command(Some(path))?, None, true))
            }
            Err(error) => Err(error),
        }
    }

    pub fn resume_session(&self, session_reference: &str) -> Result<()> {
        self.resume_session_in(session_reference, None)
    }

    /// Natively resumes a provider session under the active profile.
    ///
    /// An indexed session must belong to the active profile's agent and to the
    /// active profile itself (a session whose profile was removed is refused),
    /// and must have been recorded as natively resumable; it resumes in its
    /// recorded workspace. A reference `ContextWake` has not indexed is passed to
    /// the agent only when the agent's native resume is verified; it resumes in
    /// `workspace_path`, or the workspace of the current directory.
    pub fn resume_session_in(
        &self,
        session_reference: &str,
        workspace_path: Option<&Path>,
    ) -> Result<()> {
        let profile = self.store.active_profile()?.ok_or_else(|| {
            ContextWakeError::InvalidData(
                "no active profile; run 'ctx profile add' or 'ctx profile use'".into(),
            )
        })?;
        let known = match self.store.session(session_reference) {
            Ok(session) => Some(session),
            Err(ContextWakeError::SessionNotFound(_)) => None,
            Err(error) => return Err(error),
        };
        let adapter = self.agents.get(&profile.agent_id)?;
        let workspace = if let Some(known) = &known {
            if known.agent_id != profile.agent_id {
                return Err(ContextWakeError::Provider(format!(
                    "This session belongs to agent {}, but active profile {} uses {}. Native cross-agent resume was not attempted. Create a handoff instead.",
                    known.agent_id, profile.display_name, profile.agent_id
                )));
            }
            match known.profile_id {
                Some(id) if id == profile.id => {}
                Some(_) => {
                    return Err(ContextWakeError::Provider(
                        "This session was recorded under another profile. Native cross-profile resume was not attempted. Create a handoff instead."
                            .into(),
                    ));
                }
                None => {
                    return Err(ContextWakeError::Provider(format!(
                        "This session's ContextWake profile was removed, so it cannot be verified as belonging to profile {}. Native resume was not attempted. Create a handoff instead.",
                        profile.display_name
                    )));
                }
            }
            if known.resume_capability != ResumeCapability::Native
                || known.provider_session_id.is_none()
            {
                return Err(ContextWakeError::CapabilityUnavailable(
                    "this local session has no verified native provider session ID; continue with a workspace handoff"
                        .into(),
                ));
            }
            let workspace =
                with_conventional_path(self.store.workspace(&known.workspace_id.to_string())?);
            if let Some(path) = workspace_path {
                let requested = canonical_workspace_directory(path)?;
                if !path_is_within(&requested, &workspace.path) {
                    return Err(ContextWakeError::InvalidData(format!(
                        "this session belongs to workspace {} and resumes there; omit --workspace or pass that workspace",
                        workspace_label(&workspace)
                    )));
                }
            }
            workspace
        } else {
            let native_resume = adapter.capabilities().native_resume;
            if native_resume.support != CapabilitySupport::Verified {
                return Err(ContextWakeError::CapabilityUnavailable(format!(
                    "{} native resume is not verified ({}), so only sessions indexed by 'ctx session sync' are resumed. Native resume was not attempted; continue with a workspace handoff instead.",
                    adapter.display_name(),
                    native_resume.detail
                )));
            }
            self.workspace_for_command(workspace_path)?
        };
        ensure_workspace_directory(&workspace)?;
        let provider_session_id = known
            .as_ref()
            .and_then(|session| session.provider_session_id.as_deref())
            .unwrap_or(session_reference);
        println!(
            "Resuming {} session {} in workspace {}.",
            adapter.display_name(),
            terminal_line(provider_session_id),
            workspace_label(&workspace)
        );
        adapter.resume(&profile.agent_home, &workspace.path, provider_session_id)?;
        if let Some(known) = known {
            self.store.record_native_resume(&known.id.to_string())?;
        } else {
            let now = Utc::now();
            self.store.insert_session(&Session {
                id: Uuid::new_v4(),
                provider_session_id: Some(provider_session_id.into()),
                title: None,
                workspace_id: workspace.id,
                profile_id: Some(profile.id),
                agent_id: profile.agent_id.clone(),
                model_provider_id: profile.model_provider_id.clone(),
                model: profile.model_preference.clone(),
                started_at: now,
                last_seen_at: now,
                // Only agents with verified native resume reach this point.
                resume_capability: ResumeCapability::Native,
                archived: false,
                continuity: ContinuityKind::NativeResume,
            })?;
            self.select_after_success(&workspace);
        }
        println!(
            "Native {} session resume completed in workspace {}.",
            adapter.display_name(),
            workspace_label(&workspace)
        );
        Ok(())
    }

    fn profile_command(&self, command: ProfileCommand, json: bool) -> Result<()> {
        match command {
            ProfileCommand::Add {
                name,
                display_name,
                description,
                agent,
                model_provider,
                model,
                allow_shared_identity,
                isolated,
                existing_login,
            } => {
                let created = self.add_profile(&NewProfile {
                    name: &name,
                    display_name: display_name.as_deref(),
                    description: description.as_deref(),
                    agent_id: &agent,
                    model_provider_id: model_provider.as_deref(),
                    model_preference: model.as_deref(),
                    allow_shared_identity,
                    home: if isolated {
                        ProfileHomeChoice::Isolated
                    } else if existing_login {
                        ProfileHomeChoice::ExistingLogin
                    } else {
                        ProfileHomeChoice::Automatic
                    },
                })?;
                let profile = &created.profile;
                if let Some(warning) = self
                    .shared_identity_warning(&profile.agent_id)
                    .ok()
                    .flatten()
                {
                    warn(&warning);
                }
                if json {
                    print_json(profile);
                } else {
                    println!(
                        "{}",
                        profile_created_message(
                            &created,
                            self.agent_display_name(&profile.agent_id),
                            isolated
                        )
                    );
                }
            }
            ProfileCommand::List => {
                let profiles = self.store.list_profiles()?;
                if json {
                    print_json(&profiles);
                } else {
                    let active = self.store.active_profile()?.map(|profile| profile.id);
                    if profiles.is_empty() {
                        println!("No profiles. Create one with 'ctx profile add Personal'.");
                    }
                    for profile in profiles {
                        let active_marker = if active == Some(profile.id) { "*" } else { " " };
                        println!(
                            "{active_marker} {:<20} {:<8} {:<14} {}",
                            terminal_line(&profile.display_name),
                            profile.agent_id,
                            home_mode_tag(profile.home_mode),
                            profile.auth_state.as_str()
                        );
                    }
                }
            }
            ProfileCommand::Show { profile } => {
                let profile = self.store.profile(&profile)?;
                if json {
                    print_json(&profile);
                } else {
                    println!("{}", profile.display_name);
                    println!("  ID:       {}", profile.id);
                    println!("  Agent:    {}", profile.agent_id);
                    println!(
                        "  Provider: {}",
                        profile
                            .model_provider_id
                            .as_deref()
                            .unwrap_or("agent default")
                    );
                    println!(
                        "  Model:    {}",
                        profile
                            .model_preference
                            .as_deref()
                            .unwrap_or("agent default")
                    );
                    println!("  Auth:     {}", profile.auth_state.as_str());
                    println!(
                        "  Sign-in:  {}",
                        profile_home_summary(&profile, self.agent_display_name(&profile.agent_id))
                    );
                    println!("  Home:     {}", display_path(&profile.agent_home));
                }
            }
            ProfileCommand::Use {
                profile,
                handoff,
                objective,
                workspace,
                launch,
                new_session,
                no_conversation,
            } => {
                if launch && !handoff {
                    return Err(ContextWakeError::InvalidData(
                        "--launch starts the agent from the handoff that this switch creates; pass --handoff --launch".into(),
                    ));
                }
                if new_session && !launch {
                    return Err(ContextWakeError::InvalidData(
                        "--new-session applies to --launch; pass --handoff --launch --new-session"
                            .into(),
                    ));
                }
                // Validate the target (it exists and its agent is installed)
                // before a workspace is chosen or registered, so a typo changes
                // nothing.
                let target = self.validated_switch_target(&profile)?;
                let already_active = self
                    .store
                    .active_profile()?
                    .is_some_and(|active| active.id == target.id);
                let workspace = if handoff && (!already_active || launch) {
                    let (workspace, registration_warning) =
                        self.command_workspace(workspace.as_deref())?;
                    for warning in capture_warnings(&workspace, registration_warning) {
                        warn(&warning);
                    }
                    Some(workspace)
                } else {
                    None
                };
                let (outcome, warnings) = self.switch_to_validated_with(
                    &target,
                    workspace.as_ref(),
                    SwitchRequest {
                        create_handoff: handoff,
                        objective,
                        conversation: None,
                        refresh_when_active: launch,
                    },
                    !no_conversation,
                )?;
                for warning in &warnings {
                    warn(warning);
                }
                let handoff_workspace = workspace.filter(|_| outcome.handoff.is_some());
                if let Some(workspace) = &handoff_workspace {
                    self.select_after_success(workspace);
                }
                if let Some(warning) = self
                    .shared_identity_warning(&outcome.active.agent_id)
                    .ok()
                    .flatten()
                {
                    warn(&warning);
                }
                if !json {
                    println!("Active profile: {}", outcome.active.display_name);
                    println!("{}", outcome.message);
                    if let Some(handoff) = &outcome.handoff {
                        println!("Handoff: {}", handoff.id);
                    }
                    if let Some(workspace) = &handoff_workspace {
                        println!("Workspace: {}", workspace_label(workspace));
                    }
                }
                let launched = match (&outcome.handoff, launch) {
                    (Some(record), true) => Some(self.continue_handoff_reporting(
                        &record.id.to_string(),
                        None,
                        new_session,
                        json,
                    )?),
                    _ => None,
                };
                if json {
                    let mut value = serde_json::json!({
                        "previous_profile": outcome.previous,
                        "active_profile": outcome.active,
                        "handoff": outcome.handoff,
                        "message": outcome.message,
                        "workspace": handoff_workspace,
                    });
                    if let Some(launched) = launched {
                        value["launch"] = serde_json::to_value(launched).unwrap_or_default();
                    }
                    print_json(&value);
                }
            }
            ProfileCommand::Remove { profile, yes } => {
                require_confirmation(yes, "profile metadata removal", "--yes")?;
                // Only metadata is removed: neither a managed home nor the
                // agent's own default home is ever deleted here.
                let removed = self.store.remove_profile(&profile)?;
                match removed.home_mode {
                    ProfileHomeMode::AgentDefault => println!(
                        "Removed ContextWake metadata for {}. Your existing {} sign-in and its home {} were not changed.",
                        terminal_line(&removed.display_name),
                        self.agent_display_name(&removed.agent_id),
                        display_path(&removed.agent_home)
                    ),
                    ProfileHomeMode::Isolated => println!(
                        "Removed ContextWake metadata for {}. Provider credentials and {} were not deleted.",
                        terminal_line(&removed.display_name),
                        display_path(&removed.agent_home)
                    ),
                }
            }
            ProfileCommand::Login {
                profile,
                device_auth,
            } => {
                let profile = self.store.profile(&profile)?;
                let adapter = self.agents.get(&profile.agent_id)?;
                if let Some(warning) = self
                    .shared_identity_warning(&profile.agent_id)
                    .ok()
                    .flatten()
                {
                    warn(&warning);
                }
                if let Some(notice) = existing_login_notice(&profile, adapter.display_name(), "in")
                {
                    warn(&notice);
                }
                let auth_state = adapter.login(&profile.agent_home, device_auth)?;
                self.store.update_profile_auth(profile.id, auth_state)?;
                println!(
                    "{} authentication flow completed for {}. Recorded status: {}.",
                    adapter.display_name(),
                    profile.display_name,
                    auth_state.as_str()
                );
            }
            ProfileCommand::Logout { profile, yes } => {
                let profile = self.store.profile(&profile)?;
                let adapter = self.agents.get(&profile.agent_id)?;
                let notice = existing_login_notice(&profile, adapter.display_name(), "out");
                match &notice {
                    Some(notice) if !yes => {
                        return Err(ContextWakeError::InvalidData(format!(
                            "{notice} Signing out requires explicit confirmation; pass --yes."
                        )));
                    }
                    Some(notice) => warn(notice),
                    None => require_confirmation(yes, "provider credential removal", "--yes")?,
                }
                adapter.logout(&profile.agent_home)?;
                self.store
                    .update_profile_auth(profile.id, AuthState::SignedOut)?;
                println!(
                    "{} credentials removed for {}.",
                    adapter.display_name(),
                    profile.display_name
                );
            }
        }
        Ok(())
    }

    fn workspace_command(&self, command: WorkspaceCommand, json: bool) -> Result<i32> {
        let mut exit_code = 0;
        match command {
            WorkspaceCommand::Add { path, trust } => {
                let (workspace, warning) = self.register_workspace_reporting(
                    &path,
                    if trust {
                        TrustState::Trusted
                    } else {
                        TrustState::Untrusted
                    },
                    true,
                )?;
                if let Some(warning) = &warning {
                    warn(warning);
                }
                output_value(&workspace, json, || {
                    format!(
                        "Registered workspace {}. It is now the active workspace.",
                        workspace_label(&workspace)
                    )
                });
            }
            WorkspaceCommand::List => {
                let workspaces = self.store.list_workspaces()?;
                if json {
                    print_json(&workspaces);
                } else if workspaces.is_empty() {
                    println!("No workspaces. Add one with 'ctx workspace add .'.");
                } else {
                    let active = self.store.active_workspace()?.map(|value| value.id);
                    for workspace in workspaces {
                        let active_marker = if active == Some(workspace.id) {
                            "*"
                        } else {
                            " "
                        };
                        println!(
                            "{active_marker} {:<24} {}",
                            terminal_line(&workspace.display_name),
                            display_path(&workspace.path)
                        );
                    }
                }
            }
            WorkspaceCommand::Show { workspace } => {
                let workspace = self.store.workspace(&workspace)?;
                output_value(&workspace, json, || format!("{workspace:#?}"));
            }
            WorkspaceCommand::Open { workspace } => {
                let workspace = self.store.workspace(&workspace)?;
                // An active workspace whose directory is gone would be the
                // fallback of every later TUI start; refuse to select it.
                ensure_workspace_directory(&workspace)?;
                self.store.set_active_workspace(workspace.id)?;
                output_value(&workspace, json, || {
                    format!(
                        "Opened workspace {}. No provider or repository command was launched.",
                        workspace_label(&workspace)
                    )
                });
            }
            WorkspaceCommand::Remove { workspace, yes } => {
                require_confirmation(yes, "workspace metadata removal", "--yes")?;
                let workspace = self.store.remove_workspace(&workspace)?;
                println!(
                    "Removed workspace metadata for {}. Repository files were not changed.",
                    workspace_label(&workspace)
                );
            }
            WorkspaceCommand::Validate { workspace } => {
                let workspace = self.workspace_for_command(workspace.as_deref())?;
                let results = self.run_project_validation(&workspace)?;
                exit_code = i32::from(
                    results
                        .iter()
                        .any(|result| result.status != crate::model::ValidationStatus::Passed),
                );
                if json {
                    print_json(&results);
                } else {
                    println!("Workspace: {}", workspace_label(&workspace));
                    for result in &results {
                        println!(
                            "{:<7} {} ({})",
                            result.status.as_str().to_uppercase(),
                            result.command.join(" "),
                            result.summary
                        );
                    }
                }
            }
        }
        Ok(exit_code)
    }

    fn session_command(&self, command: SessionCommand, json: bool) -> Result<()> {
        match command {
            SessionCommand::Sync {
                workspace,
                max_count,
            } => {
                let profile = self.store.active_profile()?.ok_or_else(|| {
                    ContextWakeError::InvalidData(
                        "an active profile is required; run 'ctx profile add'".into(),
                    )
                })?;
                let workspace = self.workspace_for_command(workspace.as_deref())?;
                let adapter = self.agents.get(&profile.agent_id)?;
                let capabilities = adapter.capabilities();
                if !capabilities.session_listing.is_available() {
                    return Err(ContextWakeError::CapabilityUnavailable(format!(
                        "{} does not expose a supported session list",
                        adapter.display_name()
                    )));
                }
                let workspace_path = canonicalize_simplified(&workspace.path)
                    .unwrap_or_else(|_| workspace.path.clone());
                // --max-count limits this workspace's sessions, so filter the
                // agent's listing by workspace before applying it.
                let mut discovered = adapter
                    .list_sessions(
                        &profile.agent_home,
                        &workspace.path,
                        SESSION_DISCOVERY_LIMIT,
                    )?
                    .into_iter()
                    .filter(|item| {
                        item.workspace_path.as_deref().is_some_and(|path| {
                            let candidate = canonicalize_simplified(path)
                                .unwrap_or_else(|_| simplify_path(path));
                            paths_equivalent(&candidate, &workspace_path)
                        })
                    })
                    .collect::<Vec<_>>();
                discovered.sort_by_key(|item| Reverse(item.updated_at.or(item.created_at)));
                discovered.truncate(max_count);
                let mut matched = 0_usize;
                let mut created = 0_usize;
                for item in discovered {
                    matched += 1;
                    let now = Utc::now();
                    let (started_at, last_seen_at) = item.normalized_times(now);
                    let session = Session {
                        id: Uuid::new_v4(),
                        provider_session_id: Some(item.provider_session_id),
                        title: item
                            .title
                            .as_deref()
                            .and_then(|title| sanitize_untrusted_label(title, 200)),
                        workspace_id: workspace.id,
                        profile_id: Some(profile.id),
                        agent_id: profile.agent_id.clone(),
                        model_provider_id: profile.model_provider_id.clone(),
                        model: profile.model_preference.clone(),
                        started_at,
                        last_seen_at,
                        resume_capability: match capabilities.native_resume.support {
                            CapabilitySupport::Verified => ResumeCapability::Native,
                            CapabilitySupport::Unsupported => ResumeCapability::HandoffOnly,
                            CapabilitySupport::Partial
                            | CapabilitySupport::Experimental
                            | CapabilitySupport::Unknown => ResumeCapability::Unknown,
                        },
                        archived: false,
                        continuity: ContinuityKind::Unknown,
                    };
                    created += usize::from(self.store.upsert_provider_session(&session)?);
                }
                self.select_after_success(&workspace);
                if json {
                    print_json(&serde_json::json!({
                        "agent_id": profile.agent_id,
                        "workspace_id": workspace.id,
                        "matched": matched,
                        "created": created,
                        "updated": matched.saturating_sub(created),
                    }));
                } else {
                    println!(
                        "Indexed {matched} {} session(s) for workspace {}: {created} new, {} updated.",
                        adapter.display_name(),
                        workspace_label(&workspace),
                        matched.saturating_sub(created)
                    );
                }
            }
            SessionCommand::List {
                archived,
                workspace,
                profile,
                agent,
                query,
            } => {
                let workspace_id = workspace
                    .as_deref()
                    .map(|value| self.store.workspace(value).map(|item| item.id))
                    .transpose()?;
                let profile_id = profile
                    .as_deref()
                    .map(|value| self.store.profile(value).map(|item| item.id))
                    .transpose()?;
                let query = query.map(|value| value.to_lowercase());
                let sessions =
                    self.store
                        .list_sessions(archived)?
                        .into_iter()
                        .filter(|session| {
                            workspace_id.is_none_or(|id| session.workspace_id == id)
                                && profile_id.is_none_or(|id| session.profile_id == Some(id))
                                && agent.as_deref().is_none_or(|value| {
                                    session.agent_id.eq_ignore_ascii_case(value)
                                })
                                && query.as_deref().is_none_or(|value| {
                                    session.id.to_string().to_lowercase().contains(value)
                                        || session.title.as_deref().is_some_and(|title| {
                                            title.to_lowercase().contains(value)
                                        })
                                        || session
                                            .provider_session_id
                                            .as_deref()
                                            .is_some_and(|id| id.to_lowercase().contains(value))
                                        || session.agent_id.to_lowercase().contains(value)
                                        || session.model_provider_id.as_deref().is_some_and(
                                            |provider| provider.to_lowercase().contains(value),
                                        )
                                        || session.model.as_deref().is_some_and(|model| {
                                            model.to_lowercase().contains(value)
                                        })
                                })
                        })
                        .collect::<Vec<_>>();
                if json {
                    print_json(&sessions);
                } else if sessions.is_empty() {
                    println!(
                        "No local session metadata. Use the selected agent's native session picker where supported."
                    );
                } else {
                    for session in sessions {
                        println!(
                            "{}  {:<12} {:<28} {:<24} {}",
                            session.id,
                            session.agent_id,
                            session.title.as_deref().unwrap_or("untitled"),
                            session
                                .provider_session_id
                                .as_deref()
                                .unwrap_or("provider id unavailable"),
                            session.continuity.label()
                        );
                    }
                }
            }
            SessionCommand::Show { session } => {
                let session = self.store.session(&session)?;
                output_value(&session, json, || format!("{session:#?}"));
            }
            SessionCommand::Resume { session, workspace } => {
                self.resume_session_in(&session, workspace.as_deref())?;
            }
            SessionCommand::Archive { session, yes } => {
                require_confirmation(yes, "local session metadata archival", "--yes")?;
                let session = self.store.archive_session(&session, true)?;
                let workspace = self
                    .store
                    .workspace(&session.workspace_id.to_string())
                    .map(|workspace| format!(" in workspace {}", workspace_label(&workspace)))
                    .unwrap_or_default();
                println!(
                    "Archived local metadata for {}{workspace}. Provider session data was not deleted.",
                    session.id
                );
            }
        }
        Ok(())
    }

    fn checkpoint_command(&self, command: CheckpointCommand, json: bool) -> Result<()> {
        let service = self.checkpoints();
        match command {
            CheckpointCommand::Create {
                objective,
                task,
                notes,
                workspace,
                completed,
                decisions,
                pending,
                known_issues,
                validate,
                no_conversation,
            } => {
                let (workspace, registration_warning) =
                    self.command_workspace(workspace.as_deref())?;
                for warning in capture_warnings(&workspace, registration_warning) {
                    warn(&warning);
                }
                let profile = self.store.active_profile()?;
                let validation_results = if validate {
                    self.run_project_validation(&workspace)?
                } else {
                    Vec::new()
                };
                let (conversation, conversation_warning) = self.conversation_for_checkpoint(
                    profile.as_ref(),
                    &workspace,
                    !no_conversation,
                );
                if let Some(warning) = &conversation_warning {
                    warn(warning);
                }
                let checkpoint = service.create_with_conversation(
                    &workspace,
                    profile.as_ref(),
                    None,
                    CheckpointInput {
                        objective,
                        active_task: task,
                        completed,
                        decisions,
                        pending_tasks: pending,
                        known_issues,
                        user_notes: notes,
                        validation_results,
                    },
                    conversation,
                )?;
                self.select_after_success(&workspace);
                output_value(&checkpoint, json, || {
                    let conversation =
                        checkpoint
                            .conversation
                            .as_ref()
                            .map_or_else(String::new, |excerpt| {
                                format!(
                                    "\nIncluded the recent {} conversation excerpt (session {}).",
                                    crate::handoff::agent_label(&excerpt.agent_id),
                                    terminal_line(&excerpt.provider_session_id)
                                )
                            });
                    format!(
                        "Created checkpoint {} for workspace {} (redaction: {}){conversation}",
                        checkpoint.id,
                        workspace_label(&workspace),
                        checkpoint.redaction_status.as_str()
                    )
                });
            }
            CheckpointCommand::List => {
                let listing = service.list_with_unreadable()?;
                if json {
                    // The JSON shape stays a list of checkpoints; unreadable
                    // entries are reported on standard error.
                    print_json(&listing.checkpoints);
                } else if listing.checkpoints.is_empty() && listing.unreadable.is_empty() {
                    println!(
                        "No checkpoints. Create one with 'ctx checkpoint create --objective ...'."
                    );
                } else {
                    for checkpoint in &listing.checkpoints {
                        println!(
                            "{}  {}  {}",
                            checkpoint.id,
                            checkpoint.created_at,
                            terminal_line(&checkpoint.objective)
                        );
                    }
                }
                for unreadable in &listing.unreadable {
                    warn(&format!(
                        "checkpoint {} ({}) cannot be read: {}. Remove the stale entry with '{}'.",
                        unreadable.id,
                        terminal_line(&unreadable.objective),
                        terminal_line(&unreadable.reason),
                        unreadable.recovery_command()
                    ));
                }
            }
            CheckpointCommand::Show { checkpoint } => {
                let checkpoint = service.read(&checkpoint)?;
                output_value(&checkpoint, json, || format_checkpoint(&checkpoint));
            }
            CheckpointCommand::Delete { checkpoint, yes } => {
                require_confirmation(yes, "checkpoint deletion", "--yes")?;
                // Best effort: an unreadable checkpoint file can still be deleted.
                let workspace = service.read(&checkpoint).ok().and_then(|checkpoint| {
                    self.store
                        .workspace(&checkpoint.workspace_id.to_string())
                        .ok()
                });
                service.delete(&checkpoint)?;
                match workspace {
                    Some(workspace) => println!(
                        "Deleted checkpoint {checkpoint} of workspace {}. This cannot be recovered by ContextWake.",
                        workspace_label(&workspace)
                    ),
                    None => println!(
                        "Deleted checkpoint {checkpoint}. This cannot be recovered by ContextWake."
                    ),
                }
            }
            CheckpointCommand::Export {
                checkpoint,
                destination,
                force,
            } => {
                let path = service.export_json(&checkpoint, &destination, force)?;
                println!("Exported checkpoint to {}", display_path(&path));
            }
        }
        Ok(())
    }

    fn handoff_command(&self, command: HandoffCommand, json: bool) -> Result<()> {
        let service = self.handoffs();
        let checkpoints = self.checkpoints();
        match command {
            HandoffCommand::Create { checkpoint } => {
                let checkpoint = checkpoints.read(&checkpoint)?;
                let workspace = self.store.workspace(&checkpoint.workspace_id.to_string())?;
                let handoff = service.create(&checkpoint, &workspace)?;
                output_value(&handoff, json, || {
                    format!(
                        "Created handoff {} for workspace {}. Preview with 'ctx handoff preview {}'.",
                        handoff.id,
                        workspace_label(&workspace),
                        handoff.id
                    )
                });
            }
            HandoffCommand::List => {
                let handoffs = service.list()?;
                if json {
                    print_json(&handoffs);
                } else if handoffs.is_empty() {
                    println!("No handoffs.");
                } else {
                    for handoff in handoffs {
                        println!("{}  {}  {}", handoff.id, handoff.created_at, handoff.sha256);
                    }
                }
            }
            HandoffCommand::Show { handoff } => {
                if json {
                    print_json(&service.manifest(&handoff)?);
                } else {
                    println!("{}", service.context(&handoff)?);
                }
            }
            HandoffCommand::Preview { handoff } => {
                let manifest = service.manifest(&handoff)?;
                println!("HANDOFF PREVIEW - {}\n", manifest.handoff_id);
                println!("Security flags: {}", manifest.security_flags.join(", "));
                println!("Redactions: {:?}\n", manifest.redactions);
                println!("{}", service.context(&handoff)?);
            }
            HandoffCommand::Export {
                handoff,
                destination,
                force,
            } => {
                let path = service.export(&handoff, &destination, force)?;
                println!(
                    "Exported handoff to {}. Review all content before sharing it.",
                    display_path(&path)
                );
            }
            HandoffCommand::Import { path } => {
                let record = service.import_directory(&path)?;
                output_value(&record, json, || {
                    format!(
                        "Imported handoff {} after schema, path, size, secret, and integrity checks.",
                        record.id
                    )
                });
            }
            HandoffCommand::Continue {
                handoff,
                workspace,
                new_session,
            } => {
                let outcome = self.continue_handoff_reporting(
                    &handoff,
                    workspace.as_deref(),
                    new_session,
                    json,
                )?;
                if json {
                    print_json(&outcome);
                }
            }
        }
        Ok(())
    }

    fn agent_command(&self, command: AgentCommand, json: bool) -> Result<()> {
        match command {
            AgentCommand::List => {
                let agents = self
                    .agents
                    .implemented()
                    .map(|adapter| {
                        serde_json::json!({
                            "id": adapter.id(),
                            "name": adapter.display_name(),
                            "adapter": "implemented",
                            "capabilities": adapter.capabilities(),
                        })
                    })
                    .collect::<Vec<_>>();
                if json {
                    print_json(&agents);
                } else {
                    for agent in agents {
                        println!(
                            "{:<18} {:<24} {}",
                            agent["id"].as_str().unwrap_or("unknown"),
                            agent["name"].as_str().unwrap_or("unknown"),
                            agent["adapter"].as_str().unwrap_or("unknown")
                        );
                    }
                }
            }
            AgentCommand::Detect => {
                let active = self.store.active_profile()?;
                let active = active
                    .as_ref()
                    .map(|profile| (profile.agent_id.as_str(), profile.agent_home.as_path()));
                let health = self
                    .agents
                    .detect_all(active)
                    .into_iter()
                    .collect::<Result<Vec<_>>>()?;
                if json {
                    print_json(&health);
                } else {
                    for item in health {
                        println!(
                            "{:<18} {:<12} {}",
                            item.agent_id,
                            if item.installed {
                                "available"
                            } else {
                                "unavailable"
                            },
                            item.version.as_deref().unwrap_or(&item.message)
                        );
                    }
                }
            }
            AgentCommand::Info { agent } => {
                let adapter = self.agents.get(&agent)?;
                let active = self.store.active_profile()?;
                let home = active
                    .as_ref()
                    .filter(|profile| profile.agent_id == adapter.id())
                    .map(|profile| profile.agent_home.as_path());
                let health = adapter.detect(home)?;
                let view = crate::model::CodingAgent {
                    id: adapter.id().into(),
                    display_name: adapter.display_name().into(),
                    adapter_version: VERSION.into(),
                    detected_version: health.version,
                    executable: health.executable,
                    auth_state: health.auth_state,
                    capabilities: adapter.capabilities(),
                };
                output_value(&view, json, || format!("{view:#?}"));
            }
        }
        Ok(())
    }

    fn model_command(&self, command: ModelCommand, json: bool) -> Result<()> {
        match command {
            ModelCommand::List => {
                let profile = self.store.active_profile()?.ok_or_else(|| {
                    ContextWakeError::InvalidData(
                        "an active profile is required; run 'ctx profile add'".into(),
                    )
                })?;
                let adapter = self.agents.get(&profile.agent_id)?;
                let providers = adapter.model_providers();
                let workspace = self.store.active_workspace()?;
                let models = if adapter.capabilities().available_models.support
                    == crate::model::CapabilitySupport::Verified
                {
                    Some(adapter.available_models(
                        &profile.agent_home,
                        workspace.as_ref().map(|item| item.path.as_path()),
                        profile.model_provider_id.as_deref(),
                    )?)
                } else {
                    None
                };
                if json {
                    print_json(&serde_json::json!({
                        "agent_id": profile.agent_id,
                        "selected_provider": profile.model_provider_id,
                        "selected_model": profile.model_preference,
                        "declared_model_providers": providers,
                        "available_models": models,
                        "available_model_listing": adapter.capabilities().available_models,
                    }));
                } else {
                    println!("Agent: {}", adapter.display_name());
                    println!(
                        "Selected: {} / {}",
                        profile
                            .model_provider_id
                            .as_deref()
                            .unwrap_or("agent default"),
                        profile
                            .model_preference
                            .as_deref()
                            .unwrap_or("agent default")
                    );
                    println!("Declared model backends:");
                    for provider in providers {
                        println!(
                            "  {:<20} {}",
                            provider.id,
                            if provider.local { "LOCAL" } else { "REMOTE" }
                        );
                    }
                    if let Some(models) = models {
                        println!("Available models (agent-reported):");
                        for model in models {
                            println!("  {}/{}", model.provider_id, model.id);
                        }
                    } else {
                        println!(
                            "Model catalog: {}",
                            adapter.capabilities().available_models.detail
                        );
                    }
                }
            }
            ModelCommand::Select {
                model,
                provider,
                profile,
            } => {
                let profile = match profile {
                    Some(reference) => self.store.profile(&reference)?,
                    None => self.store.active_profile()?.ok_or_else(|| {
                        ContextWakeError::InvalidData("an active profile is required".into())
                    })?,
                };
                let adapter = self.agents.get(&profile.agent_id)?;
                if !adapter.capabilities().model_selection.is_available() {
                    return Err(ContextWakeError::CapabilityUnavailable(format!(
                        "{} does not support model selection",
                        adapter.display_name()
                    )));
                }
                let model = validate_external_reference(&model)?;
                let provider = provider
                    .map(|value| validate_external_reference(&value))
                    .transpose()?
                    .or(profile.model_provider_id.clone());
                if let Some(provider) = &provider
                    && !adapter.accepts_model_provider(provider)
                {
                    return Err(ContextWakeError::CapabilityUnavailable(format!(
                        "model provider {provider} is not declared by {}",
                        adapter.display_name()
                    )));
                }
                let (provider, model) =
                    adapter.normalize_model_selection(provider.as_deref(), &model)?;
                if adapter.capabilities().available_models.support
                    == crate::model::CapabilitySupport::Verified
                {
                    let workspace = self.store.active_workspace()?;
                    let models = adapter.available_models(
                        &profile.agent_home,
                        workspace.as_ref().map(|item| item.path.as_path()),
                        provider.as_deref(),
                    )?;
                    if !models.iter().any(|candidate| {
                        candidate.id == model
                            && provider
                                .as_deref()
                                .is_none_or(|value| candidate.provider_id == value)
                    }) {
                        return Err(ContextWakeError::InvalidData(format!(
                            "model {model} was not returned by {} for the selected provider",
                            adapter.display_name()
                        )));
                    }
                }
                let updated = self.store.update_profile_model(
                    profile.id,
                    provider.as_deref(),
                    Some(&model),
                )?;
                output_value(&updated, json, || {
                    format!(
                        "Selected model {} for profile {} (agent {}).",
                        model, updated.display_name, updated.agent_id
                    )
                });
            }
        }
        Ok(())
    }
}

/// Runs `ctx doctor` and `ctx config ...` when the application state could not
/// be initialized (an invalid configuration, a state database that cannot be
/// opened or migrated, unusable state directories), so the diagnostics and
/// repair commands that error messages point to keep working. Other commands
/// report `startup_error`.
/// The excerpt of whichever session was active most recently. A session with
/// a known last activity is newer than one without; on a tie the recorded
/// session wins. A failed read only matters when no excerpt could be read.
fn most_recent_excerpt(
    newest: Result<Option<ConversationExcerpt>>,
    recorded: Option<Result<Option<ConversationExcerpt>>>,
) -> Result<Option<ConversationExcerpt>> {
    let Some(recorded) = recorded else {
        return newest;
    };
    match (newest, recorded) {
        (Ok(Some(newest)), Ok(Some(recorded))) => Ok(Some(
            if recorded.last_activity_at >= newest.last_activity_at {
                recorded
            } else {
                newest
            },
        )),
        (Ok(Some(excerpt)), _) | (_, Ok(Some(excerpt))) => Ok(Some(excerpt)),
        (Ok(None), Ok(None)) => Ok(None),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

pub fn run_without_state(cli: Cli, startup_error: ContextWakeError) -> Result<i32> {
    match cli.command {
        Some(Command::Doctor(args)) => {
            let report = run_doctor_without_state(args.verbose);
            print_doctor(&report, cli.json, cli.ascii);
            Ok(i32::from(report.has_failures()))
        }
        Some(Command::Config(args)) => {
            let paths = AppPaths::discover()?;
            let loaded = AppConfig::read_or_create(&paths).ok();
            config_command(&paths, loaded.as_ref(), args.command, cli.json)
        }
        _ => Err(startup_error),
    }
}

pub fn print_version(binary_name: &str, json: bool) -> Result<i32> {
    if json {
        print_json(&serde_json::json!({"application": PRODUCT_NAME, "version": VERSION}));
    } else {
        println!("{binary_name} {VERSION}");
    }
    Ok(0)
}

/// Writes a completion script for the binary the user invoked (`ctx`, or the
/// `ctxwake` compatibility alias).
pub fn print_completion(shell: clap_complete::Shell, binary_name: &'static str) -> Result<i32> {
    let mut command = Cli::command().name(binary_name).bin_name(binary_name);
    clap_complete::generate(shell, &mut command, binary_name, &mut io::stdout());
    Ok(0)
}

/// `loaded` is `None` when the configuration could not be loaded at startup.
fn config_command(
    paths: &AppPaths,
    loaded: Option<&AppConfig>,
    command: ConfigCommand,
    json: bool,
) -> Result<i32> {
    match command {
        ConfigCommand::Show => print_json(&loaded_config(paths, loaded)?),
        ConfigCommand::Path => println!("{}", display_path(&paths.config_file())),
        ConfigCommand::Validate => {
            loaded_config(paths, loaded)?.validate()?;
            println!(
                "Configuration is valid: {}",
                display_path(&paths.config_file())
            );
        }
        ConfigCommand::Set {
            telemetry,
            retention_days,
            ascii,
            update_check,
            git_timeout_ms,
            validation_timeout_ms,
            capture_conversation_excerpts,
        } => {
            if telemetry.is_none()
                && retention_days.is_none()
                && ascii.is_none()
                && update_check.is_none()
                && git_timeout_ms.is_none()
                && validation_timeout_ms.is_none()
                && capture_conversation_excerpts.is_none()
            {
                return Err(ContextWakeError::Configuration(
                    "config set requires at least one setting".into(),
                ));
            }
            let mut config = match loaded {
                Some(config) => config.clone(),
                None => read_config_for_update(paths)?,
            };
            if let Some(value) = telemetry {
                config.telemetry = value;
            }
            if let Some(value) = retention_days {
                config.retention_days = value;
            }
            if let Some(value) = ascii {
                config.ascii = value;
            }
            if let Some(value) = update_check {
                config.update_check = value;
            }
            if let Some(value) = git_timeout_ms {
                config.git_timeout_ms = value;
            }
            if let Some(value) = capture_conversation_excerpts {
                config.capture_conversation_excerpts = value;
            }
            if let Some(value) = validation_timeout_ms {
                config.validation_timeout_ms = value;
            }
            config.save(paths)?;
            if json {
                print_json(&config);
            } else {
                println!(
                    "Updated non-secret configuration at {}.",
                    display_path(&paths.config_file())
                );
            }
        }
    }
    Ok(0)
}

fn loaded_config(paths: &AppPaths, loaded: Option<&AppConfig>) -> Result<AppConfig> {
    match loaded {
        Some(config) => Ok(config.clone()),
        None => AppConfig::read_or_create(paths),
    }
}

/// Reads `config.toml` for `ctx config set` without validating it, so an
/// out-of-range value can be repaired with the command the error points to.
/// The result is validated again when it is saved. A file that cannot be
/// parsed is not rewritten, because its other settings would be lost.
fn read_config_for_update(paths: &AppPaths) -> Result<AppConfig> {
    let path = paths.config_file();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    if std::fs::symlink_metadata(&path)
        .at(&path)?
        .file_type()
        .is_symlink()
    {
        return Err(ContextWakeError::UnsafePath(format!(
            "configuration file is a symlink: {}",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(&path).at(&path)?;
    toml::from_str(&content).map_err(|error| {
        ContextWakeError::Configuration(format!(
            "{} cannot be parsed, so 'ctx config set' will not rewrite it: {error}. Correct or remove the file, then retry.",
            path.display()
        ))
    })
}

#[derive(Serialize)]
struct StatusOutput<'a> {
    #[serde(flatten)]
    status: &'a StatusView,
    warnings: &'a [String],
}

fn print_status(report: &StatusReport, json: bool, ascii: bool) {
    let status = &report.view;
    if json {
        print_json(&StatusOutput {
            status,
            warnings: &report.warnings,
        });
        return;
    }
    print_status_profile(status.active_profile.as_ref(), &status.agent);
    println!(
        "  Workspace       {}",
        terminal_line(&status.workspace.display_name)
    );
    println!("  Path            {}", display_path(&status.workspace.path));
    if let Some(git) = &status.git {
        let state = if git.dirty {
            if ascii { "DIRTY" } else { "! DIRTY" }
        } else {
            "CLEAN"
        };
        println!(
            "  Git             {} - {} - {} staged / {} unstaged / {} untracked - +{} -{}",
            git.branch.as_deref().unwrap_or("detached"),
            state,
            git.staged_count,
            git.unstaged_count,
            git.untracked_count,
            git.additions,
            git.deletions
        );
    } else if report.git_unavailable {
        println!("  Git             Unavailable (see warnings)");
    } else {
        println!("  Git             Not a Git workspace");
    }
    println!("  Context         {}", status.context_information);
    println!(
        "  Local guardian  {} - {}",
        status.continuity_guardian.level.as_str().to_uppercase(),
        status.continuity_guardian.recommendation
    );
    print_status_warnings(&report.warnings);
}

/// `ctx status` without a workspace to show. The JSON keeps the shape of a
/// regular status, with `null` for everything that belongs to a workspace.
fn print_workspaceless_status(status: &WorkspacelessStatus, json: bool) {
    let active_profile = status.active_profile.as_ref();
    let agent = &status.agent;
    let warnings = &status.warnings;
    if json {
        print_json(&serde_json::json!({
            "application": PRODUCT_NAME,
            "version": VERSION,
            "active_profile": active_profile,
            "workspace": null,
            "git": null,
            "agent": agent,
            "latest_session": null,
            "context_information": STATUS_CONTEXT_INFORMATION,
            "continuity_guardian": null,
            "warnings": warnings,
        }));
        return;
    }
    print_status_profile(active_profile, agent);
    println!("  Workspace       None selected");
    print_status_warnings(warnings);
}

fn print_status_profile(active_profile: Option<&Profile>, agent: &AgentHealth) {
    let profile = active_profile.map_or("Not configured", |value| value.display_name.as_str());
    println!("{PRODUCT_NAME}");
    println!("  Active profile  {}", terminal_line(profile));
    println!(
        "  Agent           {} {}",
        agent.agent_id,
        terminal_line(agent.version.as_deref().unwrap_or("unavailable"))
    );
    if let Some(profile) = active_profile {
        println!(
            "  Sign-in         {}",
            match profile.home_mode {
                ProfileHomeMode::AgentDefault => "existing agent sign-in (agent's own home)",
                ProfileHomeMode::Isolated => "separate ContextWake sign-in (managed home)",
            }
        );
        println!(
            "  Model backend   {}",
            terminal_line(
                profile
                    .model_provider_id
                    .as_deref()
                    .unwrap_or("agent default")
            )
        );
        println!(
            "  Model           {}",
            terminal_line(profile.model_preference.as_deref().unwrap_or("not exposed"))
        );
    }
}

/// Warnings can quote repository content (for example a `project.toml` parse
/// error), so each one is a single terminal-safe line.
fn print_status_warnings(warnings: &[String]) {
    for warning in warnings {
        println!("  Warning         {}", terminal_line(warning));
    }
}

pub(crate) fn print_doctor(report: &DoctorReport, json: bool, ascii: bool) {
    if json {
        print_json(report);
        return;
    }
    println!("{PRODUCT_NAME} Doctor\n");
    for check in &report.checks {
        let status_marker = marker(check.status, ascii);
        // Summaries name paths and quote error text; each stays one line.
        println!(
            "{status_marker:<7} {:<22} {}",
            check.name,
            terminal_line(&check.summary)
        );
        if let Some(action) = &check.action {
            println!("        Action: {}", terminal_line(action));
        }
    }
}

fn format_checkpoint(checkpoint: &crate::model::Checkpoint) -> String {
    let mut output = format!(
        "Checkpoint {}\nObjective: {}\nCreated: {}\nRedaction: {}\n",
        checkpoint.id,
        checkpoint.objective,
        checkpoint.created_at,
        checkpoint.redaction_status.as_str()
    );
    if let Some(git) = &checkpoint.git {
        let _ = writeln!(
            output,
            "Git: {} - {} staged, {} unstaged, {} untracked - +{} -{}\n",
            git.branch.as_deref().unwrap_or("detached"),
            git.staged_count,
            git.unstaged_count,
            git.untracked_count,
            git.additions,
            git.deletions
        );
    }
    if !checkpoint.validation_results.is_empty() {
        let _ = writeln!(output, "Validation:");
        for result in &checkpoint.validation_results {
            let _ = writeln!(
                output,
                "  {}  {} ({})",
                result.status.as_str().to_uppercase(),
                result.command.join(" "),
                result.summary
            );
        }
    }
    output
}

/// Warnings about trusted project files for a command that captures a
/// checkpoint: the registration warning and, when it says something else, why
/// project instructions were not captured.
fn capture_warnings(workspace: &Workspace, registration_warning: Option<String>) -> Vec<String> {
    let mut warnings = registration_warning.into_iter().collect::<Vec<_>>();
    if let (_, Some(warning)) = read_project_instructions(workspace)
        && !warnings.contains(&warning)
    {
        warnings.push(warning);
    }
    warnings
}

fn unconfigured_agent_health() -> AgentHealth {
    AgentHealth {
        agent_id: "unconfigured".into(),
        installed: false,
        executable: None,
        version: None,
        auth_state: AuthState::Unknown,
        message: "no active coding-agent profile".into(),
    }
}

/// The current directory in canonical, conventional form.
fn current_directory() -> Result<PathBuf> {
    let missing = || {
        ContextWakeError::WorkspaceNotFound(
            "the current directory no longer exists; change to a project directory or pass --workspace <path>"
                .into(),
        )
    };
    let directory = std::env::current_dir().map_err(|_| missing())?;
    canonicalize_simplified(&directory).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            missing()
        } else {
            ContextWakeError::Io {
                path: directory.clone(),
                source,
            }
        }
    })
}

/// The user's home directory as the environment (`USERPROFILE` on Windows,
/// `HOME` elsewhere) and the operating system report it.
fn home_directories() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    let candidates = [
        std::env::home_dir(),
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()),
    ];
    for home in candidates.into_iter().flatten() {
        if !home.is_absolute() {
            continue;
        }
        let home = canonicalize_simplified(&home).unwrap_or_else(|_| simplify_path(&home));
        if !homes.iter().any(|known| paths_equivalent(known, &home)) {
            homes.push(home);
        }
    }
    homes
}

/// Why `path` is never chosen as a workspace implicitly, if it is the home
/// directory or a drive, share or filesystem root.
fn broad_root_reason(path: &Path, homes: &[PathBuf]) -> Option<&'static str> {
    if homes.iter().any(|home| paths_equivalent(path, home)) {
        Some("your home directory")
    } else if path.is_absolute() && path.parent().is_none() {
        Some("a drive or filesystem root")
    } else {
        None
    }
}

/// Refuses to launch or switch in a workspace whose directory is gone, with
/// the commands that recover from it.
fn ensure_workspace_directory(workspace: &Workspace) -> Result<()> {
    if workspace.path.is_dir() {
        return Ok(());
    }
    Err(ContextWakeError::WorkspaceNotFound(format!(
        "the directory of workspace {} no longer exists or is not reachable. Restore it, open another workspace with 'ctx workspace open <workspace>', or remove this one with 'ctx workspace remove {} --yes'.",
        workspace_label(workspace),
        workspace.id
    )))
}

/// Stored workspaces may predate conventional path storage; agents and Git
/// always receive the conventional form.
fn with_conventional_path(mut workspace: Workspace) -> Workspace {
    workspace.path = simplify_path(&workspace.path);
    workspace.git_root = workspace.git_root.map(|root| simplify_path(&root));
    workspace
}

/// "name (path)" for human-readable output.
fn workspace_label(workspace: &Workspace) -> String {
    format!(
        "{} ({})",
        terminal_line(&workspace.display_name),
        display_path(&workspace.path)
    )
}

/// A path for human-readable output. Directory names can contain terminal
/// escape sequences and control characters on Unix (for example from an
/// extracted archive); they must not reach the terminal.
fn display_path(path: &Path) -> String {
    terminal_line(&path.display().to_string())
}

/// Single-line, terminal-safe text: escape sequences are removed, and line
/// breaks, other control characters and bidirectional or zero-width format
/// characters are replaced with `?`.
fn terminal_line(value: &str) -> String {
    sanitize_terminal(value)
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '\u{061c}'
                        | '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
            {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Error text for warnings: terminal-safe and secret-redacted, like errors.
fn safe_message(error: &ContextWakeError) -> String {
    SecretScanner::new()
        .redact(&sanitize_terminal(&error.to_string()))
        .text
}

fn warn(message: &str) {
    eprintln!("Warning: {}", terminal_line(message));
}

fn marker(status: CheckStatus, ascii: bool) -> &'static str {
    match (status, ascii) {
        (CheckStatus::Pass, true) => "PASS",
        (CheckStatus::Warning, true) => "WARN",
        (CheckStatus::Fail, true) => "FAIL",
        (CheckStatus::Pass, false) => "[PASS]",
        (CheckStatus::Warning, false) => "[WARN]",
        (CheckStatus::Fail, false) => "[FAIL]",
    }
}

fn require_confirmation(confirmed: bool, operation: &str, flag: &str) -> Result<()> {
    if confirmed {
        Ok(())
    } else {
        Err(ContextWakeError::InvalidData(format!(
            "{operation} requires explicit confirmation; review the target and pass {flag}"
        )))
    }
}

fn output_value<T: Serialize>(value: &T, json: bool, human: impl FnOnce() -> String) {
    if json {
        print_json(value);
    } else {
        println!("{}", human());
    }
}

fn print_json<T: Serialize>(value: &T) {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    if serde_json::to_writer_pretty(&mut lock, value).is_ok() {
        let _ = writeln!(lock);
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::*;

    fn git_init(directory: &Path) {
        let status = std::process::Command::new("git")
            .current_dir(directory)
            .args(["init", "--quiet"])
            .stdin(Stdio::null())
            .status()
            .expect("Git must be installed for workspace tests");
        assert!(status.success(), "git init failed");
    }

    fn application(root: &Path, git: GitClient) -> Application {
        let paths = AppPaths::from_root(root.join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        Application {
            paths,
            config: AppConfig::default(),
            store,
            git,
            agents: AgentRegistry::discover(),
        }
    }

    fn directory(path: PathBuf) -> PathBuf {
        std::fs::create_dir_all(&path).expect("directory");
        canonicalize_simplified(&path).expect("canonical directory")
    }

    fn target_path(target: DirectoryTarget) -> PathBuf {
        match target {
            DirectoryTarget::Workspace(workspace, _) => workspace.path,
            DirectoryTarget::Refused { directory, reason } => {
                panic!("{} was refused: {reason}", directory.display())
            }
        }
    }

    fn refusal_reason(target: DirectoryTarget) -> String {
        match target {
            DirectoryTarget::Workspace(workspace, _) => {
                panic!("{} was chosen implicitly", workspace.path.display())
            }
            DirectoryTarget::Refused { reason, .. } => reason,
        }
    }

    #[test]
    fn omitted_workspace_follows_the_current_directory_rules() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let home = directory(root.path().join("home"));
        let homes = [home.clone()];

        // Rule 2: a Git work tree resolves to its top level, also from a subdirectory.
        let repository = directory(home.join("projects").join("repo"));
        git_init(&repository);
        let nested = directory(repository.join("src").join("deep"));
        assert_eq!(
            target_path(app.directory_target(&nested, &homes).expect("target")),
            repository
        );
        // Rule 4: any other directory is its own workspace.
        let notes = directory(home.join("notes"));
        assert_eq!(
            target_path(app.directory_target(&notes, &homes).expect("target")),
            notes
        );
        // Rule 1: the longest registered workspace that contains the directory wins.
        let outer = directory(root.path().join("outer"));
        let inner = directory(outer.join("inner"));
        let deeper = directory(inner.join("deeper"));
        app.register_workspace(&outer, TrustState::Untrusted, false)
            .expect("outer");
        app.register_workspace(&inner, TrustState::Untrusted, false)
            .expect("inner");
        assert_eq!(
            target_path(app.directory_target(&deeper, &homes).expect("target")),
            inner
        );
        let sibling = directory(outer.join("sibling"));
        assert_eq!(
            target_path(app.directory_target(&sibling, &homes).expect("target")),
            outer
        );
        // Rule 3: the home directory and roots are refused.
        assert!(
            refusal_reason(app.directory_target(&home, &homes).expect("target"))
                .contains("home directory")
        );
        let filesystem_root = home.ancestors().last().expect("root").to_path_buf();
        assert!(
            refusal_reason(
                app.directory_target(&filesystem_root, &homes)
                    .expect("target")
            )
            .contains("root")
        );
        // A registered home directory (earlier versions registered it
        // implicitly) is neither chosen there nor for the directories below it.
        app.register_workspace(&home, TrustState::Untrusted, false)
            .expect("home registration");
        assert!(
            refusal_reason(app.directory_target(&home, &homes).expect("target"))
                .contains("home directory")
        );
        let downloads = directory(home.join("Downloads"));
        assert_eq!(
            target_path(app.directory_target(&downloads, &homes).expect("target")),
            downloads
        );
    }

    #[test]
    fn interactive_resolution_falls_back_to_the_displayed_active_workspace_only_at_home() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let home = directory(root.path().join("home"));
        let homes = [home.clone()];
        assert!(matches!(
            app.interactive_workspace(&home, &homes),
            Err(ContextWakeError::WorkspaceNotFound(_))
        ));

        let active = directory(root.path().join("active-project"));
        app.register_workspace(&active, TrustState::Untrusted, true)
            .expect("active workspace");
        assert_eq!(
            app.interactive_workspace(&home, &homes)
                .expect("active workspace")
                .path,
            active
        );
        // Anywhere else the current directory still wins over the active workspace.
        let current = directory(root.path().join("current-project"));
        assert_eq!(
            app.interactive_workspace(&current, &homes)
                .expect("current workspace")
                .path,
            current
        );
    }

    /// Rows written by older builds hold the verbatim `\\?\` form; registering
    /// the directory again must update that row (keeping its id, trust,
    /// sessions and checkpoints) instead of inserting a duplicate.
    #[cfg(windows)]
    #[test]
    fn rows_stored_with_another_spelling_are_updated_not_duplicated() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let project = directory(root.path().join("project"));
        let verbatim = project.canonicalize().expect("verbatim path");
        assert!(verbatim.to_string_lossy().starts_with(r"\\?\"));
        let now = Utc::now();
        let stored = Workspace {
            id: Uuid::new_v4(),
            path: verbatim,
            display_name: "project".into(),
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
        app.store.upsert_workspace(&stored).expect("legacy row");

        let registered = app
            .register_workspace(&project, TrustState::Untrusted, false)
            .expect("registration");
        assert_eq!(registered.id, stored.id);
        assert_eq!(
            registered.path, project,
            "callers get the conventional form"
        );
        assert_eq!(registered.trust_state, TrustState::Trusted);
        assert_eq!(app.store.list_workspaces().expect("workspaces").len(), 1);
    }

    #[test]
    fn git_work_tree_rooted_at_home_is_not_chosen_implicitly() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let home = directory(root.path().join("home"));
        git_init(&home);
        let project = directory(home.join("project"));
        let reason = refusal_reason(app.directory_target(&project, &[home]).expect("target"));
        assert!(reason.contains("Git work tree"), "{reason}");
        assert!(reason.contains("home directory"), "{reason}");
    }

    #[test]
    fn refusal_names_the_active_workspace_and_the_explicit_option() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let project = directory(root.path().join("calculator"));
        let workspace = app
            .register_workspace(&project, TrustState::Untrusted, true)
            .expect("active workspace");
        let error = app.workspace_refusal(
            &root.path().join("home"),
            "your home directory",
            "--workspace",
        );
        assert!(matches!(error, ContextWakeError::WorkspaceNotFound(_)));
        let message = error.to_string();
        assert!(message.contains("your home directory"), "{message}");
        assert!(message.contains("--workspace <path>"), "{message}");
        assert!(message.contains("calculator"), "{message}");
        assert!(
            message.contains(&format!(
                "--workspace \"{}\"",
                display_path(&workspace.path)
            )),
            "{message}"
        );
    }

    #[test]
    fn invalid_trusted_project_configuration_degrades_to_a_warning() {
        let root = tempfile::tempdir().expect("root");
        let app = application(root.path(), GitClient::new(Duration::from_secs(10)));
        let project = directory(root.path().join("project"));
        std::fs::create_dir(project.join(".contextwake")).expect("config directory");
        std::fs::write(
            project.join(".contextwake/project.toml"),
            "schema_version = 1\nname = \"Calculator\"\n",
        )
        .expect("project config");
        let trusted = app
            .register_workspace(&project, TrustState::Trusted, false)
            .expect("trusted workspace");
        assert_eq!(trusted.display_name, "Calculator");

        // A newer ContextWake build (or a merge) adds a key this build rejects.
        std::fs::write(
            project.join(".contextwake/project.toml"),
            "schema_version = 1\nname = \"Calculator\"\nfuture_setting = true\n",
        )
        .expect("newer project config");
        let workspace = app
            .register_workspace(&project, TrustState::Untrusted, false)
            .expect("registration must not fail on repository content");
        assert_eq!(workspace.id, trusted.id);
        assert_eq!(workspace.trust_state, TrustState::Trusted);
        let (_, warning) = app
            .register_workspace_reporting(&project, TrustState::Untrusted, false)
            .expect("registration");
        assert!(
            warning.is_some_and(|warning| warning.contains("future_setting")),
            "the ignored configuration is reported"
        );
        let status = app
            .status_with_agent(&project, unconfigured_agent_health())
            .expect("status must not fail on repository content");
        assert_eq!(status.workspace.id, trusted.id);
        // Explicit validation still reports the error.
        assert!(app.run_project_validation(&workspace).is_err());
    }

    #[test]
    fn missing_git_leaves_ordinary_directories_usable() {
        let root = tempfile::tempdir().expect("root");
        let app = application(
            root.path(),
            GitClient::with_executable("contextwake-test-missing-git", Duration::from_millis(200)),
        );
        let plain = directory(root.path().join("notes"));
        let workspace = app
            .register_workspace(&plain, TrustState::Untrusted, false)
            .expect("registration without Git");
        assert_eq!(workspace.path, plain);
        assert!(workspace.git_root.is_none());
        assert_eq!(
            target_path(app.directory_target(&plain, &[]).expect("target")),
            plain
        );
        let report = app
            .status_report(workspace, unconfigured_agent_health(), Vec::new())
            .expect("status without Git");
        assert!(report.git_unavailable);
        assert!(report.view.git.is_none());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("Git state is unavailable"))
        );
    }

    #[test]
    fn terminal_output_cannot_carry_escape_sequences_or_line_breaks() {
        let hostile = PathBuf::from(
            "repo\u{1b}]0;pwned\u{7}\u{1b}[2J-name\r\nActive profile  Admin\u{202e}txt\u{200b}",
        );
        let shown = display_path(&hostile);
        for forbidden in ['\u{1b}', '\u{7}', '\n', '\r', '\u{202e}', '\u{200b}'] {
            assert!(
                !shown.contains(forbidden),
                "{shown:?} contains {forbidden:?}"
            );
        }
        assert!(shown.starts_with("repo"));
        assert!(shown.contains("-name"));
        assert_eq!(terminal_line("Calculator"), "Calculator");
    }

    mod conversation_capture {
        use std::sync::Arc;

        use super::*;
        use crate::model::{
            AgentCapabilities, DiscoveredAgentSession, ModelProvider, ProfileHomeMode,
        };
        use crate::provider::{AgentAdapter, CursorAdapter};

        /// What a fake agent's `conversation_excerpt` returns.
        #[derive(Clone, Copy)]
        enum Transcript {
            /// An excerpt of session `<agent id>-session`.
            Readable,
            /// The agent cannot read transcripts at all.
            Unsupported,
            /// Reading failed.
            Broken,
            /// Like Codex's non-interactive sessions: the agent's newest-session
            /// lookup finds nothing, but the recorded session reads by its ID.
            RecordedOnly,
        }

        struct ExcerptAgent {
            id: &'static str,
            transcript: Transcript,
        }

        impl AgentAdapter for ExcerptAgent {
            fn id(&self) -> &'static str {
                self.id
            }
            fn display_name(&self) -> &'static str {
                self.id
            }
            fn model_providers(&self) -> Vec<ModelProvider> {
                Vec::new()
            }
            fn capabilities(&self) -> AgentCapabilities {
                CursorAdapter::with_executable("unused-agent").capabilities()
            }
            fn detect(&self, _agent_home: Option<&Path>) -> Result<AgentHealth> {
                Err(ContextWakeError::CapabilityUnavailable("fake".into()))
            }
            fn auth_status(&self, _agent_home: &Path) -> Result<AuthState> {
                Ok(AuthState::Unknown)
            }
            fn initialize_profile_home(&self, _agent_home: &Path) -> Result<()> {
                Ok(())
            }
            fn login(&self, _agent_home: &Path, _device_auth: bool) -> Result<AuthState> {
                Err(ContextWakeError::CapabilityUnavailable("fake".into()))
            }
            fn logout(&self, _agent_home: &Path) -> Result<()> {
                Err(ContextWakeError::CapabilityUnavailable("fake".into()))
            }
            fn list_sessions(
                &self,
                _agent_home: &Path,
                _workspace: &Path,
                _max_count: usize,
            ) -> Result<Vec<DiscoveredAgentSession>> {
                Ok(Vec::new())
            }
            fn resume(
                &self,
                _agent_home: &Path,
                _workspace: &Path,
                _session_id: &str,
            ) -> Result<()> {
                Err(ContextWakeError::CapabilityUnavailable("fake".into()))
            }
            fn start_with_handoff(
                &self,
                _agent_home: &Path,
                _workspace: &Path,
                _handoff_directory: &Path,
                _model_provider: Option<&str>,
                _model: Option<&str>,
            ) -> Result<()> {
                Err(ContextWakeError::CapabilityUnavailable("fake".into()))
            }
            fn conversation_excerpt(
                &self,
                _agent_home: &Path,
                _workspace: &Path,
                provider_session_id: Option<&str>,
            ) -> Result<Option<ConversationExcerpt>> {
                match self.transcript {
                    Transcript::Readable => Ok(Some(ConversationExcerpt {
                        agent_id: self.id.into(),
                        provider_session_id: format!("{}-session", self.id),
                        session_title: None,
                        captured_at: Utc::now(),
                        last_activity_at: None,
                        user_requests: vec![format!("request in {}", self.id)],
                        last_agent_message: Some("done".into()),
                        truncated: false,
                    })),
                    Transcript::Unsupported => Err(ContextWakeError::CapabilityUnavailable(
                        "no transcripts".into(),
                    )),
                    Transcript::Broken => {
                        Err(ContextWakeError::Provider("transcript is corrupt".into()))
                    }
                    Transcript::RecordedOnly => {
                        Ok(provider_session_id.map(|session_id| ConversationExcerpt {
                            agent_id: self.id.into(),
                            provider_session_id: session_id.into(),
                            session_title: None,
                            captured_at: Utc::now(),
                            last_activity_at: None,
                            user_requests: vec!["do step 2".into()],
                            last_agent_message: Some("Step 2 is complete".into()),
                            truncated: false,
                        }))
                    }
                }
            }
        }

        struct Fixture {
            _root: tempfile::TempDir,
            app: Application,
            workspace: Workspace,
        }

        fn fixture(agents: &[(&'static str, Transcript)]) -> Fixture {
            let root = tempfile::tempdir().expect("root");
            let mut app = application(root.path(), GitClient::new(Duration::from_secs(10)));
            app.agents = AgentRegistry::from_adapters(
                agents
                    .iter()
                    .map(|&(id, transcript)| {
                        Arc::new(ExcerptAgent { id, transcript }) as Arc<dyn AgentAdapter>
                    })
                    .collect(),
            );
            let directory = directory(root.path().join("work space"));
            let workspace = app
                .register_workspace(&directory, TrustState::Untrusted, false)
                .expect("workspace");
            Fixture {
                _root: root,
                app,
                workspace,
            }
        }

        fn add_profile(app: &Application, name: &str, agent_id: &str) -> Profile {
            let now = Utc::now();
            let id = Uuid::new_v4();
            let profile = Profile {
                id,
                name: name.into(),
                display_name: name.into(),
                agent_id: agent_id.into(),
                model_provider_id: None,
                model_preference: None,
                description: None,
                agent_home: app.paths.agent_home(agent_id, &id),
                home_mode: ProfileHomeMode::Isolated,
                account_fingerprint: None,
                auth_state: AuthState::SignedOut,
                created_at: now,
                updated_at: now,
                last_used_at: None,
            };
            app.store.insert_profile(&profile).expect("profile");
            profile
        }

        #[test]
        fn checkpoint_capture_reads_the_profiles_agent_unless_switched_off() {
            let mut fixture = fixture(&[("readable", Transcript::Readable)]);
            let profile = add_profile(&fixture.app, "source", "readable");
            let (excerpt, warning) =
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&profile), &fixture.workspace, true);
            assert_eq!(
                excerpt.expect("captured by default").provider_session_id,
                "readable-session"
            );
            assert_eq!(warning, None);

            // `--no-conversation` alone switches it off.
            assert_eq!(
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&profile), &fixture.workspace, false),
                (None, None)
            );
            // The configuration alone switches it off.
            fixture.app.config.capture_conversation_excerpts = false;
            assert_eq!(
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&profile), &fixture.workspace, true),
                (None, None)
            );
        }

        #[test]
        fn capture_reads_the_session_contextwake_recorded_for_the_profile() {
            let fixture = fixture(&[("recorded", Transcript::RecordedOnly)]);
            let profile = add_profile(&fixture.app, "source", "recorded");
            // Without a recorded session, the agent's own lookup finds nothing.
            assert_eq!(
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&profile), &fixture.workspace, true),
                (None, None)
            );
            let now = Utc::now();
            fixture
                .app
                .store
                .insert_session(&Session {
                    id: Uuid::new_v4(),
                    provider_session_id: Some("launched-by-contextwake".into()),
                    title: None,
                    workspace_id: fixture.workspace.id,
                    profile_id: Some(profile.id),
                    agent_id: profile.agent_id.clone(),
                    model_provider_id: None,
                    model: None,
                    started_at: now,
                    last_seen_at: now,
                    resume_capability: ResumeCapability::Native,
                    archived: false,
                    continuity: ContinuityKind::RestoredFromHandoff,
                })
                .expect("session");
            let (excerpt, warning) =
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&profile), &fixture.workspace, true);
            assert_eq!(warning, None);
            let excerpt = excerpt.expect("excerpt of the recorded session");
            assert_eq!(excerpt.provider_session_id, "launched-by-contextwake");
            assert_eq!(
                excerpt.last_agent_message.as_deref(),
                Some("Step 2 is complete")
            );
        }

        #[test]
        fn the_most_recently_active_excerpt_is_kept() {
            let excerpt = |id: &str, minutes_ago: Option<i64>| ConversationExcerpt {
                agent_id: "codex".into(),
                provider_session_id: id.into(),
                session_title: None,
                captured_at: Utc::now(),
                last_activity_at: minutes_ago
                    .map(|minutes| Utc::now() - chrono::Duration::minutes(minutes)),
                user_requests: Vec::new(),
                last_agent_message: Some(id.into()),
                truncated: false,
            };
            let chosen = |newest, recorded| {
                most_recent_excerpt(newest, recorded)
                    .expect("excerpt")
                    .map(|excerpt| excerpt.provider_session_id)
            };
            assert_eq!(
                chosen(
                    Ok(Some(excerpt("newest", Some(1)))),
                    Some(Ok(Some(excerpt("recorded", Some(30)))))
                ),
                Some("newest".into())
            );
            assert_eq!(
                chosen(
                    Ok(Some(excerpt("newest", Some(30)))),
                    Some(Ok(Some(excerpt("recorded", Some(1)))))
                ),
                Some("recorded".into())
            );
            assert_eq!(
                chosen(Ok(None), Some(Ok(Some(excerpt("recorded", None))))),
                Some("recorded".into())
            );
            assert_eq!(
                chosen(Ok(Some(excerpt("newest", None))), None),
                Some("newest".into())
            );
            // A failed read of one session does not hide the other's excerpt.
            assert_eq!(
                chosen(
                    Err(ContextWakeError::Provider("corrupt".into())),
                    Some(Ok(Some(excerpt("recorded", None))))
                ),
                Some("recorded".into())
            );
            assert!(
                most_recent_excerpt(
                    Ok(None),
                    Some(Err(ContextWakeError::Provider("corrupt".into())))
                )
                .is_err()
            );
        }

        #[test]
        fn unreadable_transcripts_never_block_a_checkpoint() {
            let fixture = fixture(&[
                ("unsupported", Transcript::Unsupported),
                ("broken", Transcript::Broken),
            ]);
            let unsupported = add_profile(&fixture.app, "plain", "unsupported");
            assert_eq!(
                fixture.app.conversation_for_checkpoint(
                    Some(&unsupported),
                    &fixture.workspace,
                    true
                ),
                (None, None),
                "an agent without readable transcripts is skipped silently"
            );
            let broken = add_profile(&fixture.app, "broken", "broken");
            let (excerpt, warning) =
                fixture
                    .app
                    .conversation_for_checkpoint(Some(&broken), &fixture.workspace, true);
            assert_eq!(excerpt, None);
            let warning = warning.expect("a read failure is a warning");
            assert!(warning.contains("transcript is corrupt"), "{warning}");
        }

        fn switch_checkpoint(fixture: &Fixture, target: &Profile) -> (Checkpoint, Vec<String>) {
            let (outcome, warnings) = fixture
                .app
                .switch_to_validated_with(
                    target,
                    Some(&fixture.workspace),
                    SwitchRequest {
                        create_handoff: true,
                        ..SwitchRequest::default()
                    },
                    true,
                )
                .expect("switch");
            let handoff = outcome.handoff.expect("handoff");
            let checkpoint = fixture
                .app
                .checkpoints()
                .read(&handoff.checkpoint_id.to_string())
                .expect("switch checkpoint");
            (checkpoint, warnings)
        }

        #[test]
        fn switch_checkpoint_carries_the_source_profiles_conversation_not_the_targets() {
            let fixture = fixture(&[
                ("source-agent", Transcript::Readable),
                ("target-agent", Transcript::Readable),
            ]);
            let source = add_profile(&fixture.app, "source", "source-agent");
            let target = add_profile(&fixture.app, "target", "target-agent");
            fixture
                .app
                .store
                .set_active_profile(source.id)
                .expect("activate");
            let (checkpoint, warnings) = switch_checkpoint(&fixture, &target);
            assert!(warnings.is_empty(), "{warnings:?}");
            let excerpt = checkpoint.conversation.expect("source excerpt");
            assert_eq!(excerpt.agent_id, "source-agent");
            assert_eq!(excerpt.provider_session_id, "source-agent-session");
        }

        #[test]
        fn switch_with_an_unreadable_source_transcript_warns_and_still_hands_off() {
            let fixture = fixture(&[
                ("broken", Transcript::Broken),
                ("target-agent", Transcript::Readable),
            ]);
            let source = add_profile(&fixture.app, "source", "broken");
            let target = add_profile(&fixture.app, "target", "target-agent");
            fixture
                .app
                .store
                .set_active_profile(source.id)
                .expect("activate");
            let (checkpoint, warnings) = switch_checkpoint(&fixture, &target);
            assert_eq!(checkpoint.conversation, None);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0].contains("transcript is corrupt"),
                "{warnings:?}"
            );
        }
    }
}

/// Profiles that use an agent's existing sign-in, tested with a fake agent
/// whose default home is a temporary directory. Nothing here runs an agent or
/// reads a real agent home.
#[cfg(test)]
pub(crate) mod profile_tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model::{AgentCapabilities, ModelProvider};
    use crate::provider::{AgentAdapter, ClaudeAdapter};

    /// Agent calls the fake recorded, by kind and home.
    pub(crate) type Calls = Arc<Mutex<Vec<(&'static str, PathBuf)>>>;

    /// Claude Code with a configurable default home. Capabilities and
    /// detection come from a real adapter whose executable is missing, so
    /// nothing is ever spawned.
    pub(crate) struct ExistingLoginAgent {
        inner: ClaudeAdapter,
        default_home: Mutex<Option<PathBuf>>,
        pub(crate) calls: Calls,
    }

    impl ExistingLoginAgent {
        pub(crate) fn new(root: &Path, default_home: Option<PathBuf>) -> Arc<Self> {
            Arc::new(Self {
                inner: ClaudeAdapter::with_executable(root.join("missing-claude.exe")),
                default_home: Mutex::new(default_home),
                calls: Arc::default(),
            })
        }

        pub(crate) fn set_default_home(&self, home: Option<PathBuf>) {
            *self.default_home.lock().expect("default home") = home;
        }

        pub(crate) fn calls(&self, kind: &str) -> Vec<PathBuf> {
            self.calls
                .lock()
                .expect("calls")
                .iter()
                .filter(|(recorded, _)| *recorded == kind)
                .map(|(_, home)| home.clone())
                .collect()
        }

        fn record(&self, kind: &'static str, home: &Path) {
            self.calls
                .lock()
                .expect("calls")
                .push((kind, home.to_path_buf()));
        }
    }

    impl AgentAdapter for ExistingLoginAgent {
        fn id(&self) -> &'static str {
            "claude"
        }
        fn display_name(&self) -> &'static str {
            "Claude Code"
        }
        fn model_providers(&self) -> Vec<ModelProvider> {
            self.inner.model_providers()
        }
        fn capabilities(&self) -> AgentCapabilities {
            self.inner.capabilities()
        }
        fn detect(&self, agent_home: Option<&Path>) -> Result<AgentHealth> {
            self.inner.detect(agent_home)
        }
        fn auth_status(&self, agent_home: &Path) -> Result<AuthState> {
            self.record("auth_status", agent_home);
            Ok(AuthState::SignedIn)
        }
        fn initialize_profile_home(&self, agent_home: &Path) -> Result<()> {
            self.record("initialize", agent_home);
            std::fs::create_dir_all(agent_home).at(agent_home)
        }
        fn login(&self, agent_home: &Path, _device_auth: bool) -> Result<AuthState> {
            self.record("login", agent_home);
            Ok(AuthState::SignedIn)
        }
        fn logout(&self, agent_home: &Path) -> Result<()> {
            self.record("logout", agent_home);
            Ok(())
        }
        fn resume(&self, _: &Path, _: &Path, _: &str) -> Result<()> {
            Err(ContextWakeError::CapabilityUnavailable("fake agent".into()))
        }
        fn start_with_handoff(
            &self,
            _: &Path,
            _: &Path,
            _: &Path,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<()> {
            Err(ContextWakeError::CapabilityUnavailable("fake agent".into()))
        }
        fn default_home(&self) -> Option<PathBuf> {
            self.default_home.lock().expect("default home").clone()
        }
    }

    pub(crate) fn application_with(root: &Path, agent: Arc<ExistingLoginAgent>) -> Application {
        let paths = AppPaths::from_root(root.join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        Application {
            paths,
            config: AppConfig::default(),
            store,
            git: GitClient::with_executable("contextwake-test-missing-git", Duration::from_secs(1)),
            agents: AgentRegistry::from_adapters(vec![agent]),
        }
    }

    /// A default home with a space in its path and a file that must survive
    /// every operation unchanged.
    pub(crate) fn user_home(root: &Path) -> PathBuf {
        let home = root.join("User Home").join(".claude");
        std::fs::create_dir_all(&home).expect("default home");
        std::fs::write(home.join("settings.json"), "{\"theme\":\"dark\"}").expect("sentinel");
        home
    }

    fn assert_untouched(home: &Path) {
        let entries = std::fs::read_dir(home)
            .expect("default home still exists")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("settings.json")]);
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).expect("sentinel"),
            "{\"theme\":\"dark\"}"
        );
    }

    fn request(name: &str, home: ProfileHomeChoice) -> NewProfile<'_> {
        NewProfile {
            name,
            display_name: None,
            description: None,
            agent_id: "claude",
            model_provider_id: None,
            model_preference: None,
            allow_shared_identity: false,
            home,
        }
    }

    #[test]
    fn automatic_profiles_use_the_existing_login_once() {
        let root = tempfile::tempdir().expect("root");
        let home = user_home(root.path());
        let agent = ExistingLoginAgent::new(root.path(), Some(home.clone()));
        let app = application_with(root.path(), agent.clone());

        let created = app
            .add_profile(&request("Work", ProfileHomeChoice::Automatic))
            .expect("existing-login profile");
        let work = &created.profile;
        assert_eq!(work.home_mode, ProfileHomeMode::AgentDefault);
        assert!(paths_equivalent(&work.agent_home, &home));
        assert_eq!(work.auth_state, AuthState::SignedIn);
        assert_eq!(agent.calls("auth_status"), vec![work.agent_home.clone()]);
        assert!(
            agent.calls("initialize").is_empty(),
            "the agent's own home is never initialized"
        );
        assert!(!app.paths.agent_homes_dir().join("claude").exists());
        assert_untouched(&home);
        let stored = app.store.profile("work").expect("stored");
        assert_eq!(stored.home_mode, ProfileHomeMode::AgentDefault);
        assert_eq!(stored.agent_home, work.agent_home);
        let message = profile_created_message(&created, "Claude Code", false);
        assert!(
            message.contains("existing Claude Code sign-in"),
            "{message}"
        );
        assert!(
            message.contains("never writes into or deletes"),
            "{message}"
        );
        assert!(message.contains("User Home"), "{message}");

        // The default home is taken now: the next profile signs in separately.
        let created = app
            .add_profile(&request("Personal", ProfileHomeChoice::Automatic))
            .expect("isolated profile");
        let personal = &created.profile;
        assert_eq!(personal.home_mode, ProfileHomeMode::Isolated);
        assert_eq!(personal.auth_state, AuthState::SignedOut);
        assert_eq!(
            personal.agent_home,
            app.paths.agent_home("claude", &personal.id)
        );
        assert_eq!(agent.calls("initialize"), vec![personal.agent_home.clone()]);
        assert!(matches!(
            &created.existing_login,
            ExistingLogin::InUse { profile, .. } if profile == "Work"
        ));
        let message = profile_created_message(&created, "Claude Code", false);
        assert!(message.contains("was not used"), "{message}");
        assert!(message.contains("profile Work already uses"), "{message}");
        assert!(message.contains("ctx profile login personal"), "{message}");

        // Demanding the existing sign-in now fails and creates nothing.
        let refused = app
            .add_profile(&request("Third", ProfileHomeChoice::ExistingLogin))
            .expect_err("the default home is already used");
        assert!(matches!(
            refused,
            ContextWakeError::CapabilityUnavailable(_)
        ));
        let text = refused.to_string();
        assert!(text.contains("profile Work already uses"), "{text}");
        assert!(text.contains("--isolated"), "{text}");
        assert!(matches!(
            app.store.profile("third"),
            Err(ContextWakeError::ProfileNotFound(_))
        ));
        assert_eq!(agent.calls("initialize").len(), 1);
        assert_untouched(&home);
    }

    #[test]
    fn isolated_choice_forces_a_managed_home_and_existing_login_can_follow() {
        let root = tempfile::tempdir().expect("root");
        let home = user_home(root.path());
        let agent = ExistingLoginAgent::new(root.path(), Some(home.clone()));
        let app = application_with(root.path(), agent.clone());

        let isolated = app
            .add_profile(&request("Separate", ProfileHomeChoice::Isolated))
            .expect("isolated");
        assert_eq!(isolated.profile.home_mode, ProfileHomeMode::Isolated);
        assert!(isolated.existing_login.is_available());
        let message = profile_created_message(&isolated, "Claude Code", true);
        assert!(!message.contains("was not used"), "{message}");
        assert!(
            message.contains("separate ContextWake sign-in"),
            "{message}"
        );
        assert!(agent.calls("auth_status").is_empty());

        let existing = app
            .add_profile(&request("Mine", ProfileHomeChoice::ExistingLogin))
            .expect("existing login is still available");
        assert_eq!(existing.profile.home_mode, ProfileHomeMode::AgentDefault);
        assert_untouched(&home);

        // The compatibility entry points keep creating managed homes.
        let legacy = app
            .create_profile_for_agent("Legacy", None, None, "claude", None, None)
            .expect("legacy");
        assert_eq!(legacy.home_mode, ProfileHomeMode::Isolated);
    }

    #[test]
    fn unsupported_or_managed_default_homes_fall_back_to_a_separate_sign_in() {
        let root = tempfile::tempdir().expect("root");
        let agent = ExistingLoginAgent::new(root.path(), None);
        let app = application_with(root.path(), agent.clone());

        let refused = app
            .add_profile(&request("Work", ProfileHomeChoice::ExistingLogin))
            .expect_err("no default home");
        assert!(
            refused
                .to_string()
                .contains("cannot use an existing Claude Code sign-in"),
            "{refused}"
        );
        let created = app
            .add_profile(&request("Work", ProfileHomeChoice::Automatic))
            .expect("automatic falls back");
        assert_eq!(created.profile.home_mode, ProfileHomeMode::Isolated);
        assert_eq!(created.existing_login, ExistingLogin::Unsupported);

        // A home variable that points at a ContextWake-managed home (as in an
        // agent that ContextWake launched for another profile) is not the
        // user's own sign-in.
        agent.set_default_home(Some(created.profile.agent_home.clone()));
        assert!(matches!(
            app.existing_login("claude").expect("status"),
            ExistingLogin::Managed(_)
        ));
        let created = app
            .add_profile(&request("Other", ProfileHomeChoice::Automatic))
            .expect("automatic falls back");
        assert_eq!(created.profile.home_mode, ProfileHomeMode::Isolated);
        assert!(
            profile_created_message(&created, "Claude Code", false)
                .contains("ContextWake-managed profile home")
        );
    }

    #[test]
    fn a_failed_insert_never_deletes_the_agent_default_home() {
        let root = tempfile::tempdir().expect("root");
        let home = user_home(root.path());
        let agent = ExistingLoginAgent::new(root.path(), Some(home.clone()));
        let app = application_with(root.path(), agent.clone());
        app.add_profile(&request("Work", ProfileHomeChoice::Isolated))
            .expect("isolated");

        // The name is taken, so the insert fails after the home was chosen.
        assert!(
            app.add_profile(&request("work", ProfileHomeChoice::ExistingLogin))
                .is_err()
        );
        assert_untouched(&home);
        // The failed request never ran the agent's status command against
        // the user's normal sign-in.
        assert!(agent.calls("auth_status").is_empty());
    }

    #[test]
    fn remove_login_and_logout_respect_the_existing_login() {
        let root = tempfile::tempdir().expect("root");
        let home = user_home(root.path());
        let agent = ExistingLoginAgent::new(root.path(), Some(home.clone()));
        let app = application_with(root.path(), agent.clone());
        let profile = app
            .add_profile(&request("Work", ProfileHomeChoice::Automatic))
            .expect("profile")
            .profile;

        // Logout of the user's normal sign-in needs --yes and says why.
        let refused = app
            .profile_command(
                ProfileCommand::Logout {
                    profile: "work".into(),
                    yes: false,
                },
                false,
            )
            .expect_err("confirmation required");
        let text = refused.to_string();
        assert!(text.contains("normal Claude Code login"), "{text}");
        assert!(text.contains("--yes"), "{text}");
        assert!(agent.calls("logout").is_empty());

        app.profile_command(
            ProfileCommand::Login {
                profile: "work".into(),
                device_auth: false,
            },
            false,
        )
        .expect("login");
        assert_eq!(agent.calls("login"), vec![profile.agent_home.clone()]);
        app.profile_command(
            ProfileCommand::Logout {
                profile: "work".into(),
                yes: true,
            },
            false,
        )
        .expect("logout");
        assert_eq!(agent.calls("logout"), vec![profile.agent_home.clone()]);
        assert_eq!(
            app.store.profile("work").expect("profile").auth_state,
            AuthState::SignedOut
        );

        app.profile_command(
            ProfileCommand::Remove {
                profile: "work".into(),
                yes: true,
            },
            false,
        )
        .expect("remove");
        assert!(matches!(
            app.store.profile("work"),
            Err(ContextWakeError::ProfileNotFound(_))
        ));
        assert_untouched(&home);
        // With the profile gone, the existing sign-in is available again.
        assert!(app.existing_login("claude").expect("status").is_available());
    }

    #[test]
    fn existing_login_status_compares_homes_like_the_filesystem() {
        let root = tempfile::tempdir().expect("root");
        let managed = root.path().join("state").join("agent-homes");
        let home = root.path().join("User Home").join(".claude");
        let now = Utc::now();
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "work".into(),
            display_name: "Work".into(),
            agent_id: "claude".into(),
            model_provider_id: None,
            model_preference: None,
            description: None,
            agent_home: home.clone(),
            home_mode: ProfileHomeMode::AgentDefault,
            account_fingerprint: None,
            auth_state: AuthState::Unknown,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        assert_eq!(
            existing_login_status(None, &managed, &[]),
            ExistingLogin::Unsupported
        );
        assert_eq!(
            existing_login_status(Some(&home), &managed, &[]),
            ExistingLogin::Available(home.clone())
        );
        assert!(matches!(
            existing_login_status(Some(&home), &managed, std::slice::from_ref(&profile)),
            ExistingLogin::InUse { .. }
        ));
        assert!(matches!(
            existing_login_status(Some(&managed.join("claude").join("x")), &managed, &[]),
            ExistingLogin::Managed(_)
        ));
        #[cfg(windows)]
        {
            let other_case =
                PathBuf::from(home.to_string_lossy().to_uppercase().replace('\\', "/"));
            assert!(matches!(
                existing_login_status(Some(&other_case), &managed, std::slice::from_ref(&profile)),
                ExistingLogin::InUse { .. }
            ));
            let verbatim = PathBuf::from(format!(r"\\?\{}", home.display()));
            assert!(matches!(
                existing_login_status(Some(&verbatim), &managed, &[]),
                ExistingLogin::Available(stored) if stored == home
            ));
        }
    }
}
