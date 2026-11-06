/*
THESIS: ContextWake is a continuity console: identity, workspace evidence, and the safe next action stay visible together.
OWN-WORLD: graphite terminal surfaces, cyan focus, amber warnings, square single-line frames, and explicit text markers.
STORY: inspect real local/agent state, choose a profile deliberately, preserve project context, then continue with the selected coding agent.
FIRST VIEWPORT: compact title/status rail, profile lane, workspace/Git evidence, continuity panel, and keyboard action bar.
FORM: dense operator console benchmarked against Lazygit and GitHub CLI, with ContextWake's continuity decision as the center.
FINISH: unreviewed and undocumented is unfinished; this build ends with a reviewed, documented terminal workflow.
*/

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use uuid::Uuid;

use crate::app::{Application, StatusView};
use crate::checkpoint::CheckpointInput;
use crate::doctor::{DoctorReport, run_doctor_with_agent_results};
use crate::error::{ContextWakeError, Result};
use crate::model::{
    AgentHealth, AuthState, Capability, Checkpoint, CodingAgent, ContinuityRiskLevel,
    ContinuityRiskStatus, HandoffRecord, Profile, Session, TrustState, UsageSummary, Workspace,
};
use crate::paths::{paths_equivalent, simplify_path};
use crate::security::{SecretScanner, profile_slug, sanitize_terminal, validate_local_name};
use crate::{PRODUCT_NAME, VERSION};

const ACCENT: Color = Color::Cyan;
const WARNING: Color = Color::Yellow;
const MUTED: Color = Color::DarkGray;

// Below this size the full layout cannot show modals or the footer, so the TUI
// shows a compact view and pauses every key except Esc, Q and Ctrl+C.
const MIN_WIDTH: u16 = 42;
const MIN_HEIGHT: u16 = 12;
// How long Q (or a queued launch) waits for an in-flight provider refresh.
// Each probe is bounded, but a stuck descendant can hold a probe's pipes, so
// leaving never depends on the refresh finishing.
const EXIT_WAIT_LIMIT: Duration = Duration::from_secs(5);
// Enter is ignored this long after a launch screen opened by itself (after a
// profile switch), so a key pressed before the review was visible cannot start
// an agent.
const AUTO_OPEN_ARMING: Duration = Duration::from_millis(600);
// Events read in one batch; a longer paste continues in the next batch.
const MAX_INPUT_BATCH: usize = 2_048;
// A batch that starts this soon after a pasted batch belongs to the same paste.
const PASTE_CONTINUATION: Duration = Duration::from_millis(50);
const PROFILE_NAME_LIMIT: usize = 64;
const OBJECTIVE_LIMIT: usize = 200;
const SEARCH_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Home,
    Profiles,
    AgentCapabilities,
    Workspaces,
    Sessions,
    SessionDetail,
    Checkpoints,
    CheckpointDetail,
    Handoffs,
    HandoffPreview,
    Usage,
    Settings,
    Diagnostics,
    Onboarding,
    Authentication,
    Help,
}

impl Screen {
    const NAVIGATION: [Self; 10] = [
        Self::Home,
        Self::Profiles,
        Self::AgentCapabilities,
        Self::Workspaces,
        Self::Sessions,
        Self::Checkpoints,
        Self::Handoffs,
        Self::Usage,
        Self::Settings,
        Self::Diagnostics,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::Profiles => "PROFILES",
            Self::AgentCapabilities => "AGENT CAPABILITIES",
            Self::Workspaces => "WORKSPACES",
            Self::Sessions => "SESSIONS",
            Self::SessionDetail => "SESSION DETAIL",
            Self::Checkpoints => "CHECKPOINTS",
            Self::CheckpointDetail => "CHECKPOINT DETAIL",
            Self::Handoffs => "HANDOFFS",
            Self::HandoffPreview => "HANDOFF PREVIEW",
            Self::Usage => "USAGE / LOCAL ACTIVITY",
            Self::Settings => "SETTINGS",
            Self::Diagnostics => "DIAGNOSTICS",
            Self::Onboarding => "WELCOME",
            Self::Authentication => "AUTHENTICATION",
            Self::Help => "KEYBOARD REFERENCE",
        }
    }

    // The list a detail or launch screen shows a record of.
    const fn list(self) -> Option<Self> {
        match self {
            Self::SessionDetail => Some(Self::Sessions),
            Self::CheckpointDetail => Some(Self::Checkpoints),
            Self::HandoffPreview => Some(Self::Handoffs),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AgentFilter {
    #[default]
    All,
    Installed,
    Authenticated,
    NativeResume,
    Acp,
}

impl AgentFilter {
    const fn next(self) -> Self {
        match self {
            Self::All => Self::Installed,
            Self::Installed => Self::Authenticated,
            Self::Authenticated => Self::NativeResume,
            Self::NativeResume => Self::Acp,
            Self::Acp => Self::All,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::Authenticated => "Authenticated",
            Self::NativeResume => "Supports resume",
            Self::Acp => "Supports ACP",
        }
    }

    fn includes(self, agent: &CodingAgent) -> bool {
        match self {
            Self::All => true,
            Self::Installed => agent.detected_version.is_some(),
            Self::Authenticated => agent.auth_state == AuthState::SignedIn,
            Self::NativeResume => agent.capabilities.native_resume.is_available(),
            Self::Acp => agent.capabilities.acp.is_available(),
        }
    }
}

#[derive(Clone, Debug)]
enum InputMode {
    Normal,
    AddProfile { value: String, agent_index: usize },
    CreateCheckpoint { value: String },
    SearchSessions { value: String },
    // The profile id, never a list index: refreshes re-sort the profile list.
    ConfirmSwitch { target: Uuid },
}

impl InputMode {
    const fn title(&self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::AddProfile { .. } => Some("NEW PROFILE"),
            Self::CreateCheckpoint { .. } => Some("CREATE CHECKPOINT"),
            Self::SearchSessions { .. } => Some("SESSION SEARCH"),
            Self::ConfirmSwitch { .. } => Some("CONFIRM IDENTITY CHANGE"),
        }
    }

    fn text_mut(&mut self) -> Option<(&mut String, usize)> {
        match self {
            Self::AddProfile { value, .. } => Some((value, PROFILE_NAME_LIMIT)),
            Self::CreateCheckpoint { value } => Some((value, OBJECTIVE_LIMIT)),
            Self::SearchSessions { value } => Some((value, SEARCH_LIMIT)),
            Self::Normal | Self::ConfirmSwitch { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchAction {
    ContinueHandoff(String),
    ResumeSession(String),
}

// Local mutations. Each one runs on a worker thread, so Git, provider probes and
// file writes never freeze input or rendering. The result arrives together with
// freshly read local state.
#[derive(Clone, Debug)]
enum Action {
    CreateProfile {
        name: String,
        agent_id: String,
        agent_name: String,
    },
    CreateCheckpoint {
        workspace: Workspace,
        objective: String,
    },
    CreateHandoff {
        checkpoint: Box<Checkpoint>,
        workspace: Workspace,
    },
    OpenWorkspace {
        workspace: Workspace,
    },
    SwitchProfile {
        target: Box<Profile>,
        // Some: create a checkpoint and handoff for this workspace first.
        workspace: Option<Workspace>,
    },
}

impl Action {
    fn busy_label(&self) -> String {
        match self {
            Self::CreateProfile { name, .. } => format!("Creating profile {name}…"),
            Self::CreateCheckpoint { workspace, .. } => {
                format!("Creating a checkpoint for {}…", workspace.display_name)
            }
            Self::CreateHandoff { workspace, .. } => {
                format!("Creating a handoff for {}…", workspace.display_name)
            }
            Self::OpenWorkspace { workspace } => {
                format!("Opening {} and inspecting it…", workspace.display_name)
            }
            Self::SwitchProfile {
                target,
                workspace: Some(_),
            } => format!(
                "Switching to {}: checking the agent, saving a checkpoint and handoff…",
                target.display_name
            ),
            Self::SwitchProfile {
                target,
                workspace: None,
            } => format!("Switching to {}: checking the agent…", target.display_name),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NextView {
    Stay,
    Screen(Screen),
    // Select this handoff in the Handoffs list.
    Handoff(Uuid),
    // Open the preview of this handoff (after a switch created it).
    Preview(Uuid),
}

#[derive(Debug)]
struct ActionOutput {
    message: String,
    local: Box<LocalSnapshot>,
    next: NextView,
}

// The error is the complete, terminal-safe message for the footer.
type ActionResult = std::result::Result<ActionOutput, String>;

#[derive(Clone, Debug)]
struct Busy {
    label: String,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExitRequest {
    requested_at: Instant,
    // 0: Q or a launch, which wait a bounded time for an in-flight refresh.
    // 1: Ctrl+C or a second Q, which do not wait for the refresh.
    // 2: Ctrl+C again, which does not wait for anything.
    interrupts: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssueKind {
    Missing,
    Invalid,
}

impl IssueKind {
    const fn marker(self) -> &'static str {
        match self {
            Self::Missing => "[MISSING]",
            Self::Invalid => "[INVALID]",
        }
    }

    const fn summary(self) -> &'static str {
        match self {
            Self::Missing => "Workspace directory not found",
            Self::Invalid => "Workspace could not be inspected",
        }
    }
}

// Why the open workspace could not be inspected. The TUI keeps running and shows
// the reason with recovery steps instead of exiting.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceIssue {
    kind: IssueKind,
    reason: String,
    recovery: Vec<String>,
}

impl WorkspaceIssue {
    fn from_error(workspace: &Workspace, error: &ContextWakeError) -> Self {
        let kind = if workspace.path.is_dir() {
            IssueKind::Invalid
        } else {
            IssueKind::Missing
        };
        let mut recovery = Vec::new();
        match error {
            ContextWakeError::Configuration(_) | ContextWakeError::UntrustedConfiguration(_)
                if kind == IssueKind::Invalid =>
            {
                recovery.push(
                    "Fix or remove .contextwake/project.toml in this workspace, then press R."
                        .to_string(),
                );
            }
            ContextWakeError::Git(_) => {
                recovery.push(
                    "Check the Git installation with `ctx doctor`, then press R.".to_string(),
                );
            }
            _ => {}
        }
        recovery.push(
            "Press W to open another workspace; it is checked before it becomes active."
                .to_string(),
        );
        if !workspace.id.is_nil() {
            recovery.push(format!(
                "CLI: `ctx workspace open <id>`, or `ctx workspace remove {} --yes` (removes metadata only).",
                workspace.id
            ));
        }
        Self {
            kind,
            reason: error_text(error),
            recovery,
        }
    }
}

// The profile whose agent home the cached provider results were probed with.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProbeContext {
    agent: String,
    home: PathBuf,
}

impl ProbeContext {
    fn of(profile: &Profile) -> Self {
        Self {
            agent: profile.agent_id.clone(),
            home: profile.agent_home.clone(),
        }
    }

    fn matches(&self, profile: &Profile) -> bool {
        self.agent.eq_ignore_ascii_case(&profile.agent_id)
            && paths_equivalent(&self.home, &profile.agent_home)
    }
}

// Provider results a worker needs to build the status view after a mutation.
#[derive(Clone, Debug)]
struct ProviderCache {
    agents: Vec<CodingAgent>,
    probed_with: Option<ProbeContext>,
}

impl ProviderCache {
    fn status_health(&self, app: &Application) -> AgentHealth {
        let profile = app.store.active_profile().ok().flatten();
        cached_status_health(profile.as_ref(), &self.agents, self.probed_with.as_ref())
    }
}

// Selections are tracked by record identity across refreshes, which replace and
// re-sort every list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectionKey {
    Profile(Uuid),
    Agent(String),
    Workspace(Uuid),
    Session(Uuid),
    Checkpoint(Uuid),
    Handoff(Uuid),
}

// Everything the TUI shows that comes from local state (SQLite, managed files and
// Git), read without probing providers. Each part degrades on its own: a failure
// becomes an on-screen issue with a recovery hint instead of ending the TUI.
#[derive(Clone, Debug)]
struct LocalSnapshot {
    status: StatusView,
    workspace_issue: Option<WorkspaceIssue>,
    profiles: Vec<Profile>,
    workspaces: Vec<Workspace>,
    unavailable_workspaces: BTreeSet<Uuid>,
    sessions: Vec<Session>,
    checkpoints: Vec<Checkpoint>,
    handoffs: Vec<HandoffRecord>,
    usage: UsageSummary,
    issues: Vec<String>,
}

impl LocalSnapshot {
    // Reads local state for the active workspace, or for the current directory when
    // no workspace is active.
    fn collect(app: &Application, health: AgentHealth) -> Self {
        let mut issues = Vec::new();
        let active = app.store.active_workspace().unwrap_or_else(|error| {
            issues.push(format!(
                "The active workspace record could not be read: {}. Press W to open a workspace.",
                error_text(&error)
            ));
            None
        });
        let path = active
            .as_ref()
            .map_or_else(|| PathBuf::from("."), |workspace| workspace.path.clone());
        let (status, workspace_issue) = match app.status_with_agent(&path, health.clone()) {
            Ok(status) => (status, None),
            Err(error) => {
                // Nothing is persisted here: the active workspace stays as it is so the
                // user can repair or replace it deliberately.
                let workspace = active.unwrap_or_else(|| placeholder_workspace(&path));
                let issue = WorkspaceIssue::from_error(&workspace, &error);
                let profile = app.store.active_profile().ok().flatten();
                (degraded_status(workspace, profile, health), Some(issue))
            }
        };
        Self::with_status(app, status, workspace_issue, issues)
    }

    fn with_status(
        app: &Application,
        status: StatusView,
        mut workspace_issue: Option<WorkspaceIssue>,
        mut issues: Vec<String>,
    ) -> Self {
        let profiles = listed(
            app.store.list_profiles(),
            "Profiles",
            "ctx profile list",
            &mut issues,
        );
        let workspaces = listed(
            app.store.list_workspaces(),
            "Workspaces",
            "ctx workspace list",
            &mut issues,
        );
        let unavailable_workspaces = workspaces
            .iter()
            .filter(|workspace| !workspace.path.is_dir())
            .map(|workspace| workspace.id)
            .collect();
        let sessions = listed(
            app.store.list_sessions(false),
            "Sessions",
            "ctx session list",
            &mut issues,
        );
        let (checkpoints, damaged) = read_checkpoints(app, &mut issues);
        if let Some(issue) = workspace_issue.as_mut() {
            // An unreadable checkpoint also fails the status inspection, which reads
            // the workspace's latest checkpoint.
            for id in damaged.iter().take(3) {
                issue.recovery.push(format!(
                    "Remove the unreadable checkpoint record: `ctx checkpoint delete {id} --yes`."
                ));
            }
        }
        let handoffs = listed(
            app.handoffs().list(),
            "Handoffs",
            "ctx handoff list",
            &mut issues,
        );
        let usage = app.store.usage_summary().unwrap_or_else(|error| {
            issues.push(format!(
                "Usage counters could not be read: {}.",
                error_text(&error)
            ));
            empty_usage()
        });
        Self {
            status,
            workspace_issue,
            profiles,
            workspaces,
            unavailable_workspaces,
            sessions,
            checkpoints,
            handoffs,
            usage,
            issues,
        }
    }
}

fn listed<T>(
    result: Result<Vec<T>>,
    what: &str,
    command: &str,
    issues: &mut Vec<String>,
) -> Vec<T> {
    result.unwrap_or_else(|error| {
        issues.push(format!(
            "{what} could not be read: {}. Check with `{command}` or `ctx doctor`.",
            error_text(&error)
        ));
        Vec::new()
    })
}

// Reads every indexed checkpoint, skipping and reporting records whose file is
// missing or unreadable, so one damaged checkpoint cannot hide all the others.
fn read_checkpoints(app: &Application, issues: &mut Vec<String>) -> (Vec<Checkpoint>, Vec<Uuid>) {
    let rows = match app.store.list_checkpoint_paths() {
        Ok(rows) => rows,
        Err(error) => {
            issues.push(format!(
                "Checkpoints could not be listed: {}. Check with `ctx doctor`.",
                error_text(&error)
            ));
            return (Vec::new(), Vec::new());
        }
    };
    let service = app.checkpoints();
    let mut checkpoints = Vec::with_capacity(rows.len());
    let mut damaged = Vec::new();
    for (id, _, _, _) in rows {
        match service.read(&id.to_string()) {
            Ok(checkpoint) => checkpoints.push(checkpoint),
            Err(error) => {
                if damaged.len() < 3 {
                    issues.push(format!(
                        "Checkpoint {id} could not be read ({}). Remove the record with `ctx checkpoint delete {id} --yes`.",
                        error_text(&error)
                    ));
                }
                damaged.push(id);
            }
        }
    }
    if damaged.len() > 3 {
        issues.push(format!(
            "{} more checkpoint records could not be read.",
            damaged.len() - 3
        ));
    }
    (checkpoints, damaged)
}

fn placeholder_workspace(path: &Path) -> Workspace {
    let path = if path == Path::new(".") {
        std::env::current_dir().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let path = simplify_path(&path);
    let display_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_terminal)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "current directory".into());
    let now = Utc::now();
    Workspace {
        // Not a registered workspace; nothing may be created for it.
        id: Uuid::nil(),
        path,
        display_name,
        trust_state: TrustState::Untrusted,
        preferred_agent_id: None,
        preferred_model_provider_id: None,
        preferred_model: None,
        preferred_profile_id: None,
        last_session_id: None,
        git_root: None,
        created_at: now,
        last_opened_at: now,
    }
}

fn degraded_status(
    workspace: Workspace,
    active_profile: Option<Profile>,
    agent: AgentHealth,
) -> StatusView {
    StatusView {
        application: PRODUCT_NAME.into(),
        version: VERSION.into(),
        active_profile,
        workspace,
        git: None,
        agent,
        latest_session: None,
        context_information: "Unavailable: the workspace could not be inspected".into(),
        continuity_guardian: ContinuityRiskStatus {
            level: ContinuityRiskLevel::Warning,
            latest_checkpoint_id: None,
            checkpoint_age_seconds: None,
            workspace_changed: false,
            observed_changed_files: 0,
            observed_diff_lines: 0,
            recommendation: "Workspace state is unavailable; resolve it before switching profiles."
                .into(),
            basis: "LOCAL INDICATOR".into(),
        },
    }
}

fn empty_usage() -> UsageSummary {
    UsageSummary {
        provider_usage_available: false,
        provider_usage_message: "Unavailable: local usage counters could not be read".into(),
        local_session_count: 0,
        local_checkpoint_count: 0,
        local_handoff_count: 0,
        local_profile_switch_count: 0,
    }
}

#[derive(Clone, Debug)]
struct UiState {
    screen: Screen,
    previous_screen: Screen,
    status: StatusView,
    workspace_issue: Option<WorkspaceIssue>,
    profiles: Vec<Profile>,
    agents: Vec<CodingAgent>,
    probed_with: Option<ProbeContext>,
    workspaces: Vec<Workspace>,
    unavailable_workspaces: BTreeSet<Uuid>,
    // Every non-archived session; `sessions` is the (possibly filtered) list shown.
    all_sessions: Vec<Session>,
    sessions: Vec<Session>,
    session_query: Option<String>,
    checkpoints: Vec<Checkpoint>,
    handoffs: Vec<HandoffRecord>,
    usage: UsageSummary,
    doctor: DoctorReport,
    issues: Vec<String>,
    selected: usize,
    // Scroll position of the current list, kept between frames.
    list_offset: std::cell::Cell<usize>,
    // The area of the last frame; key handling pauses while it is too small.
    viewport: std::cell::Cell<Option<Rect>>,
    agent_filter: AgentFilter,
    input: InputMode,
    message: Option<String>,
    launch_action: Option<LaunchAction>,
    // A launch into a workspace other than the open one waits for a second Enter.
    pending_launch: Option<LaunchAction>,
    // Enter cannot launch from an automatically opened preview before this instant.
    launch_armed_at: Option<Instant>,
    busy: Option<Busy>,
    exit: Option<ExitRequest>,
    // Increases whenever a local mutation starts; a background load that was
    // started earlier is discarded instead of overwriting newer state.
    generation: u64,
    refresh_requested: bool,
    refreshing: bool,
    ascii: bool,
}

impl UiState {
    fn load(app: &Application, ascii: bool, probe: &dyn AgentProbe) -> Self {
        let mut profile_issue = None;
        let probed_profile = app.store.active_profile().unwrap_or_else(|error| {
            profile_issue = Some(format!(
                "The active profile could not be read: {}. Run `ctx profile list`, then `ctx profile use <name>`.",
                error_text(&error)
            ));
            None
        });
        let probed_with = probed_profile.as_ref().map(ProbeContext::of);
        let agent_health = probe.detect(
            app,
            probed_profile
                .as_ref()
                .map(|profile| (profile.agent_id.as_str(), profile.agent_home.as_path())),
        );
        let agents = load_agents(app, &agent_health);
        // The active profile can change while the probes run (a switch in this TUI or
        // another `ctx` process). A probe result is only applied to the profile whose
        // agent home it was taken with.
        let current_profile = app.store.active_profile().ok().flatten();
        let status_health = match (&current_profile, &probed_profile) {
            (Some(current), Some(probed))
                if current.id == probed.id && ProbeContext::of(probed).matches(current) =>
            {
                probed_agent_health(app, current, &agent_health)
            }
            _ => cached_status_health(current_profile.as_ref(), &agents, probed_with.as_ref()),
        };
        let mut local = LocalSnapshot::collect(app, status_health);
        local.issues.extend(profile_issue);
        let doctor = run_doctor_with_agent_results(
            &app.paths,
            &app.config,
            &app.store,
            &app.git,
            &app.agents,
            false,
            &agent_health,
        );
        let mut state = Self::from_parts(local, agents, probed_with, doctor, ascii);
        state.message = state.attention_message();
        state
    }

    fn from_parts(
        local: LocalSnapshot,
        agents: Vec<CodingAgent>,
        probed_with: Option<ProbeContext>,
        doctor: DoctorReport,
        ascii: bool,
    ) -> Self {
        let LocalSnapshot {
            status,
            workspace_issue,
            profiles,
            workspaces,
            unavailable_workspaces,
            sessions,
            checkpoints,
            handoffs,
            usage,
            issues,
        } = local;
        let screen = if profiles.is_empty() {
            Screen::Onboarding
        } else {
            Screen::Home
        };
        Self {
            screen,
            previous_screen: Screen::Home,
            status,
            workspace_issue,
            profiles,
            agents,
            probed_with,
            workspaces,
            unavailable_workspaces,
            all_sessions: sessions.clone(),
            sessions,
            session_query: None,
            checkpoints,
            handoffs,
            usage,
            doctor,
            issues,
            selected: 0,
            list_offset: std::cell::Cell::new(0),
            viewport: std::cell::Cell::new(None),
            agent_filter: AgentFilter::All,
            input: InputMode::Normal,
            message: None,
            launch_action: None,
            pending_launch: None,
            launch_armed_at: None,
            busy: None,
            exit: None,
            generation: 0,
            refresh_requested: false,
            refreshing: false,
            ascii,
        }
    }

    // The state shown when even the startup inspection could not run (for example
    // after a worker panic). The TUI still opens so the user can retry or quit.
    fn unavailable(ascii: bool, reason: &str) -> Self {
        let local = LocalSnapshot {
            status: degraded_status(
                placeholder_workspace(Path::new(".")),
                None,
                unconfigured_agent_health(),
            ),
            workspace_issue: Some(WorkspaceIssue {
                kind: IssueKind::Invalid,
                reason: reason.to_string(),
                recovery: vec!["Press R to retry, or quit and run `ctx doctor`.".to_string()],
            }),
            profiles: Vec::new(),
            workspaces: Vec::new(),
            unavailable_workspaces: BTreeSet::new(),
            sessions: Vec::new(),
            checkpoints: Vec::new(),
            handoffs: Vec::new(),
            usage: empty_usage(),
            issues: Vec::new(),
        };
        let doctor = DoctorReport {
            application: PRODUCT_NAME.into(),
            version: VERSION.into(),
            checks: Vec::new(),
        };
        let mut state = Self::from_parts(local, Vec::new(), None, doctor, ascii);
        state.screen = Screen::Home;
        state.message = Some(format!("{reason} Press R to retry."));
        state
    }

    fn attention_message(&self) -> Option<String> {
        if let Some(issue) = &self.workspace_issue {
            return Some(format!(
                "Workspace {} {}: {}. Press W to open another workspace; Home lists recovery steps.",
                self.status.workspace.display_name,
                issue.kind.marker(),
                issue.reason
            ));
        }
        self.issues
            .first()
            .map(|issue| format!("{issue} (D lists every local issue.)"))
    }

    fn apply_background_refresh(&mut self, mut refreshed: Self) {
        let selection = self.selection_key();
        let message = self.message.take();
        refreshed.screen = self.screen;
        refreshed.previous_screen = self.previous_screen;
        refreshed.agent_filter = self.agent_filter;
        refreshed.input = std::mem::replace(&mut self.input, InputMode::Normal);
        refreshed.launch_action = self.launch_action.take();
        refreshed.pending_launch = self.pending_launch.take();
        refreshed.launch_armed_at = self.launch_armed_at;
        refreshed.busy = self.busy.take();
        refreshed.exit = self.exit;
        refreshed.generation = self.generation;
        refreshed.refresh_requested = self.refresh_requested;
        refreshed.refreshing = false;
        refreshed.selected = self.selected;
        refreshed.list_offset.set(self.list_offset.get());
        refreshed.viewport.set(self.viewport.get());
        refreshed.session_query = self.session_query.take();
        refreshed.apply_session_filter();
        refreshed.message = match message {
            Some(message) if !message.starts_with("Checking local and provider") => Some(message),
            _ => refreshed
                .attention_message()
                .or_else(|| Some("Refreshed local and provider state.".into())),
        };
        *self = refreshed;
        self.close_stale_confirmation();
        self.restore_selection(selection);
    }

    fn apply_local(&mut self, local: LocalSnapshot) {
        let selection = self.selection_key();
        let LocalSnapshot {
            status,
            workspace_issue,
            profiles,
            workspaces,
            unavailable_workspaces,
            sessions,
            checkpoints,
            handoffs,
            usage,
            issues,
        } = local;
        self.status = status;
        self.workspace_issue = workspace_issue;
        self.profiles = profiles;
        self.workspaces = workspaces;
        self.unavailable_workspaces = unavailable_workspaces;
        self.all_sessions = sessions;
        self.apply_session_filter();
        self.checkpoints = checkpoints;
        self.handoffs = handoffs;
        self.usage = usage;
        self.issues = issues;
        self.close_stale_confirmation();
        self.restore_selection(selection);
    }

    // The session search is part of the view, so every refresh re-applies it to the
    // fresh list instead of silently replacing the filtered results.
    fn apply_session_filter(&mut self) {
        match &self.session_query {
            Some(query) => {
                self.sessions = self
                    .all_sessions
                    .iter()
                    .filter(|session| session_matches(session, query))
                    .cloned()
                    .collect();
            }
            None => self.sessions.clone_from(&self.all_sessions),
        }
    }

    fn close_stale_confirmation(&mut self) {
        if let InputMode::ConfirmSwitch { target } = self.input
            && !self.profiles.iter().any(|profile| profile.id == target)
        {
            self.input = InputMode::Normal;
            self.message =
                Some("The profile being confirmed no longer exists; nothing was changed.".into());
        }
    }

    fn visible_agents(&self) -> Vec<&CodingAgent> {
        self.agents
            .iter()
            .filter(|agent| self.agent_filter.includes(agent))
            .collect()
    }

    fn selection_key(&self) -> Option<SelectionKey> {
        match self.screen {
            Screen::Profiles | Screen::Authentication => self
                .profiles
                .get(self.selected)
                .map(|profile| SelectionKey::Profile(profile.id)),
            Screen::AgentCapabilities => self
                .visible_agents()
                .get(self.selected)
                .map(|agent| SelectionKey::Agent(agent.id.clone())),
            Screen::Workspaces => self
                .workspaces
                .get(self.selected)
                .map(|workspace| SelectionKey::Workspace(workspace.id)),
            Screen::Sessions | Screen::SessionDetail => self
                .sessions
                .get(self.selected)
                .map(|session| SelectionKey::Session(session.id)),
            Screen::Checkpoints | Screen::CheckpointDetail => self
                .checkpoints
                .get(self.selected)
                .map(|checkpoint| SelectionKey::Checkpoint(checkpoint.id)),
            Screen::Handoffs | Screen::HandoffPreview => self
                .handoffs
                .get(self.selected)
                .map(|handoff| SelectionKey::Handoff(handoff.id)),
            _ => None,
        }
    }

    fn index_of(&self, key: &SelectionKey) -> Option<usize> {
        match key {
            SelectionKey::Profile(id) => self.profiles.iter().position(|item| item.id == *id),
            SelectionKey::Agent(id) => self
                .visible_agents()
                .iter()
                .position(|agent| agent.id == *id),
            SelectionKey::Workspace(id) => self.workspaces.iter().position(|item| item.id == *id),
            SelectionKey::Session(id) => self.sessions.iter().position(|item| item.id == *id),
            SelectionKey::Checkpoint(id) => self.checkpoints.iter().position(|item| item.id == *id),
            SelectionKey::Handoff(id) => self.handoffs.iter().position(|item| item.id == *id),
        }
    }

    fn restore_selection(&mut self, key: Option<SelectionKey>) {
        if let Some(index) = key.as_ref().and_then(|key| self.index_of(key)) {
            self.selected = index;
            return;
        }
        if key.is_some()
            && let Some(list) = self.screen.list()
        {
            // The record on a detail or launch screen is gone. Never show (or launch)
            // a different record in its place.
            self.screen = list;
            self.pending_launch = None;
            self.launch_armed_at = None;
            self.message = Some(if self.launch_action.take().is_some() {
                // A launch queued for the vanished record is cancelled, not redirected.
                self.exit = None;
                "The selected record is no longer available; nothing was launched.".into()
            } else {
                "The selected record is no longer available.".into()
            });
        }
        self.selected = self.selected.min(self.current_len().saturating_sub(1));
    }

    fn current_len(&self) -> usize {
        match self.screen {
            Screen::Profiles => self.profiles.len(),
            Screen::AgentCapabilities => self
                .agents
                .iter()
                .filter(|agent| self.agent_filter.includes(agent))
                .count(),
            Screen::Workspaces => self.workspaces.len(),
            Screen::Sessions | Screen::SessionDetail => self.sessions.len(),
            Screen::Checkpoints | Screen::CheckpointDetail => self.checkpoints.len(),
            Screen::Handoffs | Screen::HandoffPreview => self.handoffs.len(),
            _ => 1,
        }
    }

    fn select_screen(&mut self, screen: Screen) {
        self.previous_screen = self.screen;
        self.screen = screen;
        self.selected = 0;
        self.list_offset.set(0);
        self.message = None;
        self.pending_launch = None;
        self.launch_armed_at = None;
    }

    fn provider_cache(&self) -> ProviderCache {
        ProviderCache {
            agents: self.agents.clone(),
            probed_with: self.probed_with.clone(),
        }
    }

    fn is_tiny(&self) -> bool {
        self.viewport
            .get()
            .is_some_and(|area| area.width < MIN_WIDTH || area.height < MIN_HEIGHT)
    }

    fn workspace_by_id(&self, id: Uuid) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == id)
    }

    // The workspace `continue_handoff` launches in: the one of the handoff's
    // checkpoint.
    fn handoff_workspace(&self, handoff: &HandoffRecord) -> Option<&Workspace> {
        let checkpoint = self
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == handoff.checkpoint_id)?;
        self.workspace_by_id(checkpoint.workspace_id)
    }

    fn workspace_label(&self, id: Uuid) -> String {
        let Some(workspace) = self.workspace_by_id(id) else {
            return "[WORKSPACE UNKNOWN]".into();
        };
        let mut label = workspace.display_name.clone();
        if workspace.id != self.status.workspace.id {
            label.push_str(" [OTHER WORKSPACE]");
        }
        if self.unavailable_workspaces.contains(&workspace.id) {
            label.push_str(" [MISSING]");
        }
        label
    }

    fn handoff_workspace_label(&self, handoff: &HandoffRecord) -> String {
        if handoff.checkpoint_id.is_nil() {
            return "[IMPORTED]".into();
        }
        self.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id == handoff.checkpoint_id)
            .map_or_else(
                || "[WORKSPACE UNKNOWN]".into(),
                |checkpoint| self.workspace_label(checkpoint.workspace_id),
            )
    }

    // The open workspace, when local records may be created for it.
    fn usable_workspace(&mut self, purpose: &str) -> Option<Workspace> {
        if let Some(issue) = &self.workspace_issue {
            self.message = Some(format!(
                "Cannot {purpose}: workspace {} is {} ({}). Press W to open another workspace.",
                self.status.workspace.display_name,
                issue.kind.marker(),
                issue.reason
            ));
            return None;
        }
        Some(self.status.workspace.clone())
    }

    // A launch into a workspace other than the open one needs a second Enter.
    fn confirm_other_workspace(&mut self, action: &LaunchAction, workspace: &Workspace) -> bool {
        if workspace.id == self.status.workspace.id || self.pending_launch.as_ref() == Some(action)
        {
            self.pending_launch = None;
            return true;
        }
        self.pending_launch = Some(action.clone());
        self.message = Some(format!(
            "[OTHER WORKSPACE] This starts the agent in {} ({}), not in the open workspace {}. Press Enter again to continue there, or Esc.",
            workspace.display_name,
            display_path(&workspace.path),
            self.status.workspace.display_name
        ));
        false
    }

    fn queue_selected_handoff(&mut self) -> bool {
        let Some(handoff) = self.handoffs.get(self.selected) else {
            self.message = Some("Select a handoff before starting a new session.".into());
            return false;
        };
        if handoff.checkpoint_id.is_nil() {
            self.message = Some(format!(
                "Imported handoff: run `ctx handoff continue {} --workspace <path>`.",
                handoff.id
            ));
            return false;
        }
        let action = LaunchAction::ContinueHandoff(handoff.id.to_string());
        if self.status.active_profile.is_none() {
            self.message =
                Some("Create or activate a profile (P) before starting a session.".into());
            return false;
        }
        let Some(workspace) = self.handoff_workspace(handoff).cloned() else {
            self.message = Some(format!(
                "The workspace of this handoff is unknown because its checkpoint could not be read. Run `ctx handoff continue {} --workspace <path>`.",
                handoff.id
            ));
            return false;
        };
        if self.unavailable_workspaces.contains(&workspace.id) {
            self.message = Some(format!(
                "Cannot start: workspace {} ({}) is missing.",
                workspace.display_name,
                display_path(&workspace.path)
            ));
            return false;
        }
        if !self.confirm_other_workspace(&action, &workspace) {
            return false;
        }
        self.launch_action = Some(action);
        true
    }

    fn queue_selected_session(&mut self) -> bool {
        let Some(session) = self.sessions.get(self.selected) else {
            self.message = Some("Select a session before requesting native resume.".into());
            return false;
        };
        if session.resume_capability != crate::model::ResumeCapability::Native
            || session.provider_session_id.is_none()
        {
            self.message = Some(
                "Native resume is unavailable for this record. Create a workspace handoff instead."
                    .into(),
            );
            return false;
        }
        // The same guards resume_session applies after the TUI exits, checked while
        // the user can still act on the message.
        let Some(profile) = self.status.active_profile.as_ref() else {
            self.message =
                Some("Create or activate a profile (P) before resuming a session.".into());
            return false;
        };
        if session.agent_id != profile.agent_id {
            self.message = Some(format!(
                "This session belongs to agent {}, but active profile {} uses {}. Native cross-agent resume is not attempted; create a handoff instead.",
                session.agent_id, profile.display_name, profile.agent_id
            ));
            return false;
        }
        if session.profile_id.is_some_and(|id| id != profile.id) {
            self.message = Some(
                "This session was recorded under another profile. Native cross-profile resume is not attempted; create a handoff instead."
                    .into(),
            );
            return false;
        }
        let action = LaunchAction::ResumeSession(session.id.to_string());
        let Some(workspace) = self.workspace_by_id(session.workspace_id).cloned() else {
            self.message = Some(
                "The workspace of this session is unknown; resume it with `ctx session resume <id>`."
                    .into(),
            );
            return false;
        };
        if self.unavailable_workspaces.contains(&workspace.id) {
            self.message = Some(format!(
                "Cannot resume: workspace {} ({}) is missing.",
                workspace.display_name,
                display_path(&workspace.path)
            ));
            return false;
        }
        if !self.confirm_other_workspace(&action, &workspace) {
            return false;
        }
        self.launch_action = Some(action);
        true
    }

    fn request_exit(&mut self, now: Instant) {
        self.exit = Some(ExitRequest {
            requested_at: now,
            interrupts: 0,
        });
        if self.refreshing {
            self.message = Some(if self.launch_action.is_some() {
                "Finishing bounded provider probes before launch… Esc cancels the launch.".into()
            } else {
                "Finishing bounded provider probes before exit… Q again or Ctrl+C quits now.".into()
            });
        }
    }

    // Ctrl+C: leave without launching anything. A running local action is allowed
    // to finish unless Ctrl+C is pressed again.
    fn interrupt(&mut self, now: Instant) {
        self.launch_action = None;
        self.pending_launch = None;
        let mut exit = self.exit.unwrap_or(ExitRequest {
            requested_at: now,
            interrupts: 0,
        });
        exit.interrupts = exit.interrupts.saturating_add(1);
        self.exit = Some(exit);
        if self.busy.is_some() && exit.interrupts < 2 {
            self.message = Some(
                "Quitting after the current action finishes… Ctrl+C again quits immediately."
                    .into(),
            );
        }
    }

    fn request_refresh(&mut self) {
        // R also clears an active session search.
        let cleared = self.session_query.take().is_some();
        if cleared {
            self.apply_session_filter();
            self.selected = 0;
            self.list_offset.set(0);
        }
        if self.refreshing || self.refresh_requested {
            self.message = Some(if cleared {
                "Session search cleared. Provider discovery is already running…".into()
            } else {
                "Provider discovery is already running…".into()
            });
        } else {
            self.refresh_requested = true;
            self.message = Some("Checking local and provider state…".into());
        }
    }

    fn launch_blocked_until_armed(&mut self, now: Instant) -> bool {
        match self.launch_armed_at {
            Some(armed_at) if now < armed_at => {
                self.message = Some(
                    "Review the handoff first, then press Enter again to start the NEW session."
                        .into(),
                );
                true
            }
            _ => {
                self.launch_armed_at = None;
                false
            }
        }
    }

    fn begin_action(&mut self, action: &Action, now: Instant) {
        self.busy = Some(Busy {
            label: action.busy_label(),
            started_at: now,
        });
        self.pending_launch = None;
        // A state load already in flight read the state before this mutation.
        self.generation = self.generation.wrapping_add(1);
    }

    fn finish_action(&mut self, result: ActionResult, now: Instant) {
        self.busy = None;
        self.refresh_requested = true;
        let output = match result {
            Ok(output) => output,
            Err(message) => {
                // A prompt stays open with its value so the user can correct it.
                self.message = Some(message);
                return;
            }
        };
        self.input = InputMode::Normal;
        self.apply_local(*output.local);
        match output.next {
            NextView::Stay => {}
            NextView::Screen(screen) => {
                if self.screen != screen {
                    self.previous_screen = self.screen;
                    self.screen = screen;
                }
                self.selected = self.selected.min(self.current_len().saturating_sub(1));
            }
            NextView::Handoff(id) => {
                if let Some(index) = self.handoffs.iter().position(|handoff| handoff.id == id) {
                    self.selected = index;
                }
            }
            NextView::Preview(id) => {
                if let Some(index) = self.handoffs.iter().position(|handoff| handoff.id == id) {
                    self.previous_screen = Screen::Profiles;
                    self.screen = Screen::HandoffPreview;
                    self.selected = index;
                    self.launch_armed_at = Some(now + AUTO_OPEN_ARMING);
                }
            }
        }
        self.message = Some(output.message);
    }

    // Handles one input and returns the local action to run, if any. Every failure
    // becomes an on-screen message; nothing here ends the TUI except an explicit
    // quit or launch request.
    fn dispatch(&mut self, app: &Application, input: Input, now: Instant) -> Option<Action> {
        if let Input::Key(key) = &input
            && is_interrupt(key)
        {
            self.interrupt(now);
            return None;
        }
        if self.busy.is_some() {
            // The busy dialog says input is paused; keys are dropped, never queued.
            return None;
        }
        if self.exit.is_some() {
            self.handle_exit_wait_input(&input);
            return None;
        }
        if self.is_tiny() {
            self.handle_tiny_input(&input, now);
            return None;
        }
        match input {
            Input::Paste(text) => {
                self.handle_paste(&text);
                None
            }
            Input::Key(key) => self.handle_key(app, key, now),
        }
    }

    fn handle_exit_wait_input(&mut self, input: &Input) {
        let Input::Key(key) = input else {
            return;
        };
        match key.code {
            KeyCode::Esc if self.exit.is_some_and(|exit| exit.interrupts == 0) => {
                self.exit = None;
                self.message = Some(if self.launch_action.take().is_some() {
                    "Launch cancelled; no agent was started.".into()
                } else {
                    "Exit cancelled.".into()
                });
            }
            KeyCode::Char('q' | 'Q') => {
                // A second Q stops waiting for the refresh and drops a queued launch.
                self.launch_action = None;
                if let Some(exit) = self.exit.as_mut() {
                    exit.interrupts = exit.interrupts.saturating_add(1);
                }
            }
            _ => {}
        }
    }

    fn handle_tiny_input(&mut self, input: &Input, now: Instant) {
        if let Input::Key(key) = input {
            let modal = !matches!(self.input, InputMode::Normal);
            match key.code {
                KeyCode::Esc if modal => {
                    self.input = InputMode::Normal;
                    self.message = Some("Cancelled.".into());
                    return;
                }
                KeyCode::Char('q' | 'Q') if !modal => {
                    self.request_exit(now);
                    return;
                }
                _ => {}
            }
        }
        let size = self.viewport.get().unwrap_or_default();
        self.message = Some(format!(
            "Terminal too small ({}x{}): enlarge it to {MIN_WIDTH}x{MIN_HEIGHT}. Only Esc and Q work until then.",
            size.width, size.height
        ));
    }

    // Pasted text only ever becomes prompt text: it is never submitted, never runs a
    // hotkey and never answers a dialog.
    fn handle_paste(&mut self, text: &str) {
        let line = paste_line(text);
        if line.trim().is_empty() {
            return;
        }
        if let Some((value, limit)) = self.input.text_mut() {
            if !push_text(value, &line, limit) {
                self.message = Some(format!("Pasted text was shortened to {limit} characters."));
            }
        } else {
            self.message = Some(
                "Pasted text was ignored: pasted or typed-ahead keys never run commands or answer dialogs."
                    .into(),
            );
        }
    }

    fn handle_key(&mut self, app: &Application, key: KeyEvent, now: Instant) -> Option<Action> {
        // Windows reports every key release as its own event; only presses act.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match &self.input {
            InputMode::Normal => self.handle_normal_key(app, key, now),
            InputMode::ConfirmSwitch { target } => {
                let target = *target;
                self.handle_confirm_key(key, target)
            }
            InputMode::AddProfile { .. }
            | InputMode::CreateCheckpoint { .. }
            | InputMode::SearchSessions { .. } => self.handle_prompt_key(key),
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => self.input = InputMode::Normal,
            KeyCode::Enter => return self.submit_prompt(),
            KeyCode::Backspace => {
                if let Some((value, _)) = self.input.text_mut() {
                    value.pop();
                }
            }
            KeyCode::Tab => {
                if let InputMode::AddProfile { agent_index, .. } = &mut self.input
                    && !self.agents.is_empty()
                {
                    *agent_index = (*agent_index + 1) % self.agents.len();
                }
            }
            _ => {
                if let Some(character) = text_input(&key)
                    && let Some((value, limit)) = self.input.text_mut()
                    && value.chars().count() < limit
                {
                    value.push(character);
                }
            }
        }
        None
    }

    fn submit_prompt(&mut self) -> Option<Action> {
        match &self.input {
            InputMode::AddProfile { .. } => self.submit_profile(),
            InputMode::CreateCheckpoint { value } => {
                let objective = value.trim().to_string();
                if objective.is_empty() {
                    self.message = Some("Checkpoint objective cannot be empty.".into());
                    return None;
                }
                let workspace = self.usable_workspace("create a checkpoint")?;
                Some(Action::CreateCheckpoint {
                    workspace,
                    objective,
                })
            }
            InputMode::SearchSessions { value } => {
                let query = value.trim().to_lowercase();
                self.apply_search(query);
                None
            }
            InputMode::Normal | InputMode::ConfirmSwitch { .. } => None,
        }
    }

    // Checks the name the way create_profile_for_agent will, so a rejected name keeps
    // the prompt open with an explanation instead of failing later.
    fn submit_profile(&mut self) -> Option<Action> {
        let InputMode::AddProfile { value, agent_index } = &self.input else {
            return None;
        };
        let name = value.trim().to_string();
        let agent = self.agents.get(*agent_index).cloned();
        if name.is_empty() {
            self.message = Some("Profile name cannot be empty.".into());
            return None;
        }
        let slug = match validate_local_name(&name).and_then(|_| profile_slug(&name)) {
            Ok(slug) => slug,
            Err(error) => {
                self.message = Some(format!(
                    "Profile name not accepted: {}. Edit it and press Enter again.",
                    validation_text(&error)
                ));
                return None;
            }
        };
        if self
            .profiles
            .iter()
            .any(|profile| profile.name.eq_ignore_ascii_case(&slug))
        {
            self.message = Some(format!(
                "A profile named {slug} already exists. Choose another name."
            ));
            return None;
        }
        let Some(agent) = agent else {
            self.message = Some("No coding-agent adapter is available.".into());
            return None;
        };
        Some(Action::CreateProfile {
            name,
            agent_id: agent.id,
            agent_name: agent.display_name,
        })
    }

    fn apply_search(&mut self, query: String) {
        self.input = InputMode::Normal;
        self.selected = 0;
        self.list_offset.set(0);
        if query.is_empty() {
            self.session_query = None;
            self.apply_session_filter();
            self.message = Some("Session search cleared.".into());
        } else {
            self.session_query = Some(query);
            self.apply_session_filter();
            self.message = Some(format!(
                "{} matching local session(s). Press R to clear.",
                self.sessions.len()
            ));
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent, target: Uuid) -> Option<Action> {
        let with_handoff = match key.code {
            KeyCode::Esc => {
                self.input = InputMode::Normal;
                return None;
            }
            KeyCode::Enter => true,
            KeyCode::Char('x' | 'X') => false,
            _ => return None,
        };
        let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.id == target)
            .cloned()
        else {
            self.input = InputMode::Normal;
            self.message = Some("That profile no longer exists; nothing was changed.".into());
            return None;
        };
        let workspace = if with_handoff {
            if let Some(issue) = &self.workspace_issue {
                // The dialog stays open: X (switch without an artifact) and Esc still work.
                self.message = Some(format!(
                    "No checkpoint can be created while workspace {} is {}. Press X to switch without a continuity artifact, or Esc.",
                    self.status.workspace.display_name,
                    issue.kind.marker()
                ));
                return None;
            }
            Some(self.status.workspace.clone())
        } else {
            None
        };
        self.input = InputMode::Normal;
        Some(Action::SwitchProfile {
            target: Box::new(profile),
            workspace,
        })
    }

    fn handle_normal_key(
        &mut self,
        app: &Application,
        key: KeyEvent,
        now: Instant,
    ) -> Option<Action> {
        match key.code {
            KeyCode::Char('q' | 'Q') => self.request_exit(now),
            KeyCode::Char('?') => self.select_screen(Screen::Help),
            KeyCode::Char('g' | 'G') => self.select_screen(Screen::Home),
            KeyCode::Char('p' | 'P') => self.select_screen(Screen::Profiles),
            KeyCode::Char('i' | 'I') => self.select_screen(Screen::AgentCapabilities),
            KeyCode::Char('w' | 'W') => self.select_screen(Screen::Workspaces),
            KeyCode::Char('s' | 'S') => self.select_screen(Screen::Sessions),
            KeyCode::Char('c' | 'C') => self.select_screen(Screen::Checkpoints),
            KeyCode::Char('o' | 'O') => self.select_screen(Screen::Handoffs),
            KeyCode::Char('u' | 'U') => self.select_screen(Screen::Usage),
            KeyCode::Char('d' | 'D') => self.select_screen(Screen::Diagnostics),
            KeyCode::Char('t' | 'T') => self.select_screen(Screen::Settings),
            KeyCode::Char('r' | 'R') => self.request_refresh(),
            KeyCode::Char('f' | 'F') if self.screen == Screen::AgentCapabilities => {
                self.agent_filter = self.agent_filter.next();
                self.selected = 0;
                self.list_offset.set(0);
                self.message = Some(format!("Agent filter: {}.", self.agent_filter.label()));
            }
            KeyCode::Esc => {
                self.pending_launch = None;
                self.launch_armed_at = None;
                if matches!(
                    self.screen,
                    Screen::SessionDetail
                        | Screen::CheckpointDetail
                        | Screen::HandoffPreview
                        | Screen::Help
                        | Screen::Authentication
                ) {
                    self.screen = self.previous_screen;
                } else {
                    self.screen = Screen::Home;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.pending_launch = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let length = self.current_len();
                if length > 0 {
                    self.selected = (self.selected + 1).min(length - 1);
                }
                self.pending_launch = None;
            }
            KeyCode::Char('a' | 'A')
                if matches!(self.screen, Screen::Profiles | Screen::Onboarding) =>
            {
                self.input = InputMode::AddProfile {
                    value: String::new(),
                    agent_index: first_detected_agent(self),
                };
            }
            KeyCode::Char('n' | 'N') if self.screen == Screen::Checkpoints => {
                if self.usable_workspace("create a checkpoint").is_some() {
                    self.input = InputMode::CreateCheckpoint {
                        value: String::new(),
                    };
                }
            }
            KeyCode::Char('n' | 'N') if self.screen == Screen::Handoffs => {
                return self.new_handoff_action();
            }
            KeyCode::Char('/') if self.screen == Screen::Sessions => {
                self.input = InputMode::SearchSessions {
                    value: String::new(),
                };
            }
            KeyCode::Char('l' | 'L') if self.screen == Screen::Profiles => {
                self.previous_screen = Screen::Profiles;
                self.screen = Screen::Authentication;
            }
            KeyCode::Enter => return self.handle_enter(app, now),
            _ => {}
        }
        None
    }

    fn new_handoff_action(&mut self) -> Option<Action> {
        let workspace = self.usable_workspace("create a handoff")?;
        let Some(checkpoint) = self
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.workspace_id == workspace.id)
        else {
            self.message = Some("Create a checkpoint before creating a handoff.".into());
            return None;
        };
        Some(Action::CreateHandoff {
            checkpoint: Box::new(checkpoint.clone()),
            workspace,
        })
    }

    fn handle_enter(&mut self, app: &Application, now: Instant) -> Option<Action> {
        match self.screen {
            Screen::Profiles => {
                if let Some(profile) = self.profiles.get(self.selected) {
                    let active = self.status.active_profile.as_ref().map(|value| value.id);
                    if active == Some(profile.id) {
                        self.message = Some(format!("{} is already active.", profile.display_name));
                    } else {
                        self.input = InputMode::ConfirmSwitch { target: profile.id };
                    }
                }
            }
            Screen::Workspaces => {
                // The worker inspects the directory before the active workspace changes.
                if let Some(workspace) = self.workspaces.get(self.selected) {
                    return Some(Action::OpenWorkspace {
                        workspace: workspace.clone(),
                    });
                }
            }
            Screen::Sessions => {
                self.previous_screen = Screen::Sessions;
                self.screen = Screen::SessionDetail;
            }
            Screen::SessionDetail => {
                if self.queue_selected_session() {
                    self.request_exit(now);
                }
            }
            Screen::Checkpoints => {
                self.previous_screen = Screen::Checkpoints;
                self.screen = Screen::CheckpointDetail;
            }
            Screen::Handoffs => {
                self.previous_screen = Screen::Handoffs;
                self.screen = Screen::HandoffPreview;
            }
            Screen::HandoffPreview => {
                if self.launch_blocked_until_armed(now) || !self.handoff_passes_integrity_check(app)
                {
                    return None;
                }
                if self.queue_selected_handoff() {
                    self.request_exit(now);
                }
            }
            Screen::Onboarding => {
                self.input = InputMode::AddProfile {
                    value: "Personal".into(),
                    agent_index: first_detected_agent(self),
                };
            }
            _ => {}
        }
        None
    }

    // Checks the package before the TUI exits, so a damaged handoff is reported here
    // instead of after the terminal was restored. continue_handoff validates it again
    // right before launch.
    fn handoff_passes_integrity_check(&mut self, app: &Application) -> bool {
        let Some(handoff) = self.handoffs.get(self.selected) else {
            return true;
        };
        if handoff.checkpoint_id.is_nil() {
            return true;
        }
        let id = handoff.id.to_string();
        match app.handoffs().validate(&id) {
            Ok(_) => true,
            Err(error) => {
                self.message = Some(format!(
                    "This handoff cannot be started: {}. Inspect it with `ctx handoff preview {id}`.",
                    error_text(&error)
                ));
                false
            }
        }
    }
}

fn load_agents(
    app: &Application,
    agent_health: &[crate::error::Result<crate::model::AgentHealth>],
) -> Vec<CodingAgent> {
    app.agents
        .implemented()
        .zip(agent_health)
        .map(|(adapter, health)| {
            let health = health_for_adapter(adapter, health);
            CodingAgent {
                id: adapter.id().into(),
                display_name: adapter.display_name().into(),
                adapter_version: VERSION.into(),
                detected_version: health.version,
                executable: health.executable,
                auth_state: health.auth_state,
                capabilities: adapter.capabilities(),
            }
        })
        .collect()
}

fn health_for_adapter(
    adapter: &dyn crate::provider::AgentAdapter,
    health: &crate::error::Result<crate::model::AgentHealth>,
) -> crate::model::AgentHealth {
    health
        .as_ref()
        .cloned()
        .unwrap_or_else(|error| crate::model::AgentHealth {
            agent_id: adapter.id().into(),
            installed: false,
            executable: None,
            version: None,
            auth_state: AuthState::Unknown,
            message: format!("probe failed: {error}"),
        })
}

fn probed_agent_health(
    app: &Application,
    profile: &Profile,
    agent_health: &[Result<AgentHealth>],
) -> AgentHealth {
    app.agents
        .implemented()
        .zip(agent_health)
        .find(|(adapter, _)| adapter.id() == profile.agent_id)
        .map_or_else(unconfigured_agent_health, |(adapter, health)| {
            health_for_adapter(adapter, health)
        })
}

fn unconfigured_agent_health() -> crate::model::AgentHealth {
    crate::model::AgentHealth {
        agent_id: "unconfigured".into(),
        installed: false,
        executable: None,
        version: None,
        auth_state: AuthState::Unknown,
        message: "no active coding-agent profile".into(),
    }
}

fn coding_agent_health(agent: &CodingAgent) -> crate::model::AgentHealth {
    crate::model::AgentHealth {
        agent_id: agent.id.clone(),
        installed: agent.detected_version.is_some(),
        executable: agent.executable.clone(),
        version: agent.detected_version.clone(),
        auth_state: agent.auth_state,
        message: "cached TUI provider state".into(),
    }
}

// Agent health for the status view, built from cached probe results. A result
// probed with another profile's agent home (or with no home) says nothing about
// this profile's sign-in, so it is reported with `installed: false`, which keeps
// status_with_agent from persisting it, and with the profile's own stored auth
// state until the next background refresh probes this profile's home.
fn cached_status_health(
    profile: Option<&Profile>,
    agents: &[CodingAgent],
    probed_with: Option<&ProbeContext>,
) -> AgentHealth {
    let Some(profile) = profile else {
        return unconfigured_agent_health();
    };
    let cached = agents.iter().find(|agent| agent.id == profile.agent_id);
    if let Some(agent) = cached
        && probed_with.is_some_and(|context| context.matches(profile))
    {
        return coding_agent_health(agent);
    }
    AgentHealth {
        agent_id: profile.agent_id.clone(),
        installed: false,
        executable: cached.and_then(|agent| agent.executable.clone()),
        version: cached.and_then(|agent| agent.detected_version.clone()),
        auth_state: profile.auth_state,
        message: "awaiting background provider refresh for this profile".into(),
    }
}

fn first_detected_agent(state: &UiState) -> usize {
    state
        .agents
        .iter()
        .position(|agent| agent.detected_version.is_some())
        .unwrap_or(0)
}

trait EventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read(&mut self) -> io::Result<Event>;
}

struct TerminalEvents;

impl EventSource for TerminalEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        event::read()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Key(KeyEvent),
    Paste(String),
}

// Reads the event that is ready within `timeout` plus every event already queued
// behind it.
fn read_input_batch(
    events: &mut impl EventSource,
    timeout: Duration,
) -> io::Result<Option<Vec<Event>>> {
    if !events.poll(timeout)? {
        return Ok(None);
    }
    let mut batch = vec![events.read()?];
    while batch.len() < MAX_INPUT_BATCH && events.poll(Duration::ZERO)? {
        batch.push(events.read()?);
    }
    Ok(Some(batch))
}

// Drops input that queued up while an action ran. Returns whether Ctrl+C was among
// it, because an interrupt must never be lost.
fn discard_queued_input(events: &mut impl EventSource) -> io::Result<bool> {
    let mut interrupted = false;
    for _ in 0..MAX_INPUT_BATCH {
        if !events.poll(Duration::ZERO)? {
            break;
        }
        if let Event::Key(key) = events.read()? {
            interrupted |= is_interrupt(&key);
        }
    }
    Ok(interrupted)
}

// Turns one batch of terminal events into inputs.
//
// * Only key presses act. Windows also reports every release as its own event.
// * A bracketed paste arrives as `Event::Paste`.
// * Windows consoles never deliver bracketed pastes to crossterm: pasted text
//   arrives as many key presses that are queued together, with each line break as
//   Enter. Several presses in one batch (other than held navigation keys) are
//   therefore treated as pasted text, and so is type-ahead that queued up while
//   the TUI was busy; neither may run hotkeys, answer a dialog or confirm a launch.
// * Ctrl+C always acts, even inside such a batch.
//
// The second value is true when the batch was treated as a paste.
fn classify_batch(batch: Vec<Event>, continuing_paste: bool) -> (Vec<Input>, bool) {
    let mut presses = Vec::new();
    let mut pasted = String::new();
    let mut explicit_paste = false;
    for event in batch {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(character) = pasted_char(&key) {
                    pasted.push(character);
                }
                presses.push(key);
            }
            Event::Paste(text) => {
                pasted.push_str(&text);
                explicit_paste = true;
            }
            _ => {}
        }
    }
    if let Some(interrupt) = presses.iter().copied().find(is_interrupt) {
        return (vec![Input::Key(interrupt)], false);
    }
    let burst = presses.len() > 1 && !presses.iter().all(is_pass_through_key);
    if explicit_paste || burst || (continuing_paste && !presses.is_empty()) {
        return (vec![Input::Paste(pasted)], true);
    }
    (presses.into_iter().map(Input::Key).collect(), false)
}

fn is_interrupt(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && matches!(key.code, KeyCode::Char('c' | 'C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

// Keys that are harmless when several arrive together (held arrow keys or a
// repeating Backspace); every other multi-key batch is treated as text.
fn is_pass_through_key(key: &KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Backspace
            | KeyCode::Delete
    )
}

// The character a key press types into a text prompt. Windows reports AltGr as
// Ctrl+Alt, so a character with both modifiers is text (for example @, \, { or
// ł on European layouts); Ctrl without Alt is a shortcut.
fn text_input(key: &KeyEvent) -> Option<char> {
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    ((alt || !control) && !character.is_control()).then_some(character)
}

fn pasted_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        // Kept so stray bracketed-paste markers form escape sequences, which
        // sanitize_terminal removes.
        KeyCode::Esc => Some('\u{1b}'),
        _ => text_input(key),
    }
}

// Pasted text becomes one line of plain text: terminal escape sequences and
// control characters are removed and every run of whitespace (line breaks
// included) becomes one space.
fn paste_line(text: &str) -> String {
    let mut line = String::new();
    let mut pending_space = false;
    for character in sanitize_terminal(text).chars() {
        if character.is_whitespace() {
            pending_space = true;
        } else if !character.is_control() {
            if pending_space {
                line.push(' ');
                pending_space = false;
            }
            line.push(character);
        }
    }
    if pending_space {
        line.push(' ');
    }
    line
}

// Appends as much of `text` as fits; returns false when it was shortened.
fn push_text(value: &mut String, text: &str, limit: usize) -> bool {
    let text = if value.is_empty() {
        text.trim_start()
    } else {
        text
    };
    let mut count = value.chars().count();
    for character in text.chars() {
        if count >= limit {
            return false;
        }
        value.push(character);
        count += 1;
    }
    true
}

// A worker thread whose result is polled without blocking. Dropping it detaches
// the thread instead of joining it, so quitting never waits on a stuck probe.
struct Task<T> {
    receiver: mpsc::Receiver<T>,
    worker: Option<thread::JoinHandle<()>>,
}

enum TaskPoll<T> {
    Pending,
    Ready(T),
    // The worker stopped without a result (it panicked).
    Failed,
}

impl<T: Send + 'static> Task<T> {
    fn spawn(name: &str, job: impl FnOnce() -> T + Send + 'static) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                // The receiver is gone when the TUI already quit; the result is dropped.
                let _ = sender.send(job());
            })
            .map_err(|error| {
                ContextWakeError::InvalidData(format!(
                    "could not start the TUI worker {name}: {error}"
                ))
            })?;
        Ok(Self {
            receiver,
            worker: Some(worker),
        })
    }

    fn poll(&mut self) -> TaskPoll<T> {
        let outcome = match self.receiver.try_recv() {
            Ok(value) => TaskPoll::Ready(value),
            Err(TryRecvError::Empty) => return TaskPoll::Pending,
            Err(TryRecvError::Disconnected) => TaskPoll::Failed,
        };
        if let Some(worker) = self.worker.take() {
            // The job has finished, so this join returns promptly.
            let _ = worker.join();
        }
        outcome
    }
}

// Provider discovery, injectable so tests never run real coding agents.
trait AgentProbe: Send + Sync {
    fn detect(&self, app: &Application, active: Option<(&str, &Path)>) -> Vec<Result<AgentHealth>>;
}

struct ProviderProbe;

impl AgentProbe for ProviderProbe {
    fn detect(&self, app: &Application, active: Option<(&str, &Path)>) -> Vec<Result<AgentHealth>> {
        app.agents.detect_all(active)
    }
}

struct StateLoad {
    task: Task<UiState>,
    generation: u64,
}

fn spawn_state_load(
    app: &Application,
    ascii: bool,
    probe: &Arc<dyn AgentProbe>,
    generation: u64,
) -> Result<StateLoad> {
    let app = app.clone();
    let probe = Arc::clone(probe);
    let task = Task::spawn("contextwake-tui-load", move || {
        UiState::load(&app, ascii, probe.as_ref())
    })?;
    Ok(StateLoad { task, generation })
}

// Runs one local mutation on the worker and reads the resulting local state. Every
// error becomes a complete message for the footer.
fn execute_action(app: &Application, action: Action, cache: &ProviderCache) -> ActionResult {
    match action {
        Action::CreateProfile {
            name,
            agent_id,
            agent_name,
        } => {
            let profile = app
                .create_profile_for_agent(&name, Some(&name), None, &agent_id, None, None)
                .map_err(|error| {
                    format!(
                        "Could not create profile {name}: {}. Edit the name and press Enter again.",
                        error_text(&error)
                    )
                })?;
            Ok(ActionOutput {
                message: format!(
                    "Created {} for {agent_name}. Authenticate with: ctx profile login {}",
                    profile.display_name, profile.name
                ),
                local: Box::new(LocalSnapshot::collect(app, cache.status_health(app))),
                next: NextView::Screen(Screen::Profiles),
            })
        }
        Action::CreateCheckpoint {
            workspace,
            objective,
        } => {
            let failure = |error: &ContextWakeError| {
                format!(
                    "Could not create a checkpoint for {}: {}.",
                    workspace.display_name,
                    error_text(error)
                )
            };
            let profile = app
                .store
                .active_profile()
                .map_err(|error| failure(&error))?;
            let checkpoint = app
                .checkpoints()
                .create(
                    &workspace,
                    profile.as_ref(),
                    None,
                    CheckpointInput {
                        objective,
                        ..CheckpointInput::default()
                    },
                )
                .map_err(|error| failure(&error))?;
            Ok(ActionOutput {
                message: format!(
                    "Created checkpoint {} for {} ({}).",
                    checkpoint.id,
                    workspace.display_name,
                    display_path(&workspace.path)
                ),
                local: Box::new(LocalSnapshot::collect(app, cache.status_health(app))),
                next: NextView::Stay,
            })
        }
        Action::CreateHandoff {
            checkpoint,
            workspace,
        } => {
            let handoff = app
                .handoffs()
                .create(&checkpoint, &workspace)
                .map_err(|error| {
                    format!(
                        "Could not create a handoff for {}: {}.",
                        workspace.display_name,
                        error_text(&error)
                    )
                })?;
            Ok(ActionOutput {
                message: format!(
                    "Created handoff {} for {}. Press Enter to preview.",
                    handoff.id, workspace.display_name
                ),
                local: Box::new(LocalSnapshot::collect(app, cache.status_health(app))),
                next: NextView::Handoff(handoff.id),
            })
        }
        Action::OpenWorkspace { workspace } => {
            let failure = |error: &ContextWakeError| {
                let mut message = format!(
                    "Could not open {} ({}): {}. The active workspace was not changed.",
                    workspace.display_name,
                    display_path(&workspace.path),
                    error_text(error)
                );
                if !workspace.path.is_dir() {
                    let _ = write!(
                        message,
                        " Remove the stale record with `ctx workspace remove {} --yes`.",
                        workspace.id
                    );
                }
                message
            };
            // Inspect first: the active-workspace pointer is only persisted for a
            // workspace that can actually be opened.
            let status = app
                .status_with_agent(&workspace.path, cache.status_health(app))
                .map_err(|error| failure(&error))?;
            app.store
                .set_active_workspace(status.workspace.id)
                .map_err(|error| failure(&error))?;
            let opened = status.workspace.clone();
            Ok(ActionOutput {
                message: format!(
                    "Opened {} ({}). No repository command was executed.",
                    opened.display_name,
                    display_path(&opened.path)
                ),
                local: Box::new(LocalSnapshot::with_status(app, status, None, Vec::new())),
                next: NextView::Stay,
            })
        }
        Action::SwitchProfile { target, workspace } => {
            let with_handoff = workspace.is_some();
            let outcome = app
                .switch_active_profile(
                    &target.id.to_string(),
                    workspace.as_ref(),
                    with_handoff,
                    None,
                )
                .map_err(|error| {
                    format!(
                        "Could not switch to {}: {}",
                        target.display_name,
                        error_text(&error)
                    )
                })?;
            let preview = outcome.handoff.as_ref().map(|handoff| handoff.id);
            let message = if with_handoff {
                format!(
                    "{} Review the handoff, then press Enter to start a NEW session.",
                    outcome.message
                )
            } else {
                outcome.message
            };
            Ok(ActionOutput {
                message,
                local: Box::new(LocalSnapshot::collect(app, cache.status_health(app))),
                next: preview.map_or(NextView::Stay, NextView::Preview),
            })
        }
    }
}

// The terminal modes the TUI changes; a trait so the restore logic is testable
// without a real console.
trait TerminalModes: Send + Sync {
    fn enable_raw_mode(&self) -> io::Result<()>;
    fn disable_raw_mode(&self) -> io::Result<()>;
    fn enter_alternate_screen(&self) -> io::Result<()>;
    fn leave_alternate_screen(&self) -> io::Result<()>;
    fn enable_bracketed_paste(&self) -> io::Result<()>;
    fn disable_bracketed_paste(&self) -> io::Result<()>;
    fn show_cursor(&self) -> io::Result<()>;
}

struct CrosstermModes;

impl TerminalModes for CrosstermModes {
    fn enable_raw_mode(&self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_alternate_screen(&self) -> io::Result<()> {
        execute!(io::stdout(), EnterAlternateScreen)
    }

    fn leave_alternate_screen(&self) -> io::Result<()> {
        execute!(io::stdout(), LeaveAlternateScreen)
    }

    fn enable_bracketed_paste(&self) -> io::Result<()> {
        execute!(io::stdout(), EnableBracketedPaste)
    }

    fn disable_bracketed_paste(&self) -> io::Result<()> {
        execute!(io::stdout(), DisableBracketedPaste)
    }

    fn show_cursor(&self) -> io::Result<()> {
        execute!(io::stdout(), Show)
    }
}

struct ModeState {
    modes: Box<dyn TerminalModes>,
    ui_thread: thread::ThreadId,
    raw: AtomicBool,
    alternate: AtomicBool,
    paste: AtomicBool,
    cursor: AtomicBool,
}

impl ModeState {
    // Undoes every mode that was (or may have been) enabled. Each step is attempted
    // even when an earlier one fails, and each runs at most once.
    fn restore(&self) -> io::Result<()> {
        let steps: [(&AtomicBool, &dyn Fn() -> io::Result<()>); 4] = [
            (&self.paste, &|| self.modes.disable_bracketed_paste()),
            (&self.alternate, &|| self.modes.leave_alternate_screen()),
            (&self.raw, &|| self.modes.disable_raw_mode()),
            (&self.cursor, &|| self.modes.show_cursor()),
        ];
        let mut first_error = None;
        for (enabled, undo) in steps {
            if enabled.swap(false, Ordering::SeqCst)
                && let Err(error) = undo()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

// Owns the terminal modes the TUI enables and puts them back on every exit path:
// a normal return, an error during setup or teardown, and (through a panic hook)
// a panic on the UI thread, so the shell is never left in raw mode on the
// alternate screen.
struct TerminalSession {
    state: Arc<ModeState>,
    previous_hook: Option<Arc<PanicHook>>,
}

impl TerminalSession {
    fn start(modes: Box<dyn TerminalModes>) -> io::Result<Self> {
        let mut session = Self {
            state: Arc::new(ModeState {
                modes,
                ui_thread: thread::current().id(),
                raw: AtomicBool::new(false),
                alternate: AtomicBool::new(false),
                paste: AtomicBool::new(false),
                cursor: AtomicBool::new(false),
            }),
            previous_hook: None,
        };
        session.install_panic_hook();
        let state = Arc::clone(&session.state);
        // Each flag is raised before its step runs, so a step that fails part-way is
        // still undone when `session` drops on the error path.
        state.raw.store(true, Ordering::SeqCst);
        state.modes.enable_raw_mode()?;
        state.cursor.store(true, Ordering::SeqCst);
        state.alternate.store(true, Ordering::SeqCst);
        state.modes.enter_alternate_screen()?;
        // Best effort: legacy Windows consoles reject bracketed paste, and pastes
        // there are recognised as key bursts instead (see classify_batch).
        state.paste.store(true, Ordering::SeqCst);
        let _ = state.modes.enable_bracketed_paste();
        Ok(session)
    }

    fn install_panic_hook(&mut self) {
        let previous: Arc<PanicHook> = Arc::new(std::panic::take_hook());
        let chained = Arc::clone(&previous);
        let state = Arc::clone(&self.state);
        std::panic::set_hook(Box::new(move |info| {
            // Only a panic on the UI thread ends the TUI. Worker panics are caught and
            // reported inside it, so the terminal must stay as it is for them.
            if thread::current().id() == state.ui_thread {
                let _ = state.restore();
            }
            let previous: &(dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync) = &**chained;
            previous(info);
        }));
        self.previous_hook = Some(previous);
    }

    fn uninstall_panic_hook(&mut self) {
        // take_hook and set_hook panic while unwinding; the hook then simply stays
        // installed, and its restore has nothing left to do.
        if thread::panicking() {
            return;
        }
        if let Some(previous) = self.previous_hook.take() {
            drop(std::panic::take_hook());
            std::panic::set_hook(Box::new(move |info| {
                let previous: &(dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync) = &**previous;
                previous(info);
            }));
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        let result = self.state.restore();
        self.uninstall_panic_hook();
        result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn run(app: Application, ascii: bool) -> Result<()> {
    let mut terminal_session =
        TerminalSession::start(Box::new(CrosstermModes)).map_err(terminal_error)?;
    let outcome = run_in_terminal(&app, ascii);
    let restored = terminal_session.restore();
    let launch = outcome?;
    restored.map_err(terminal_error)?;
    match launch {
        Some(LaunchAction::ContinueHandoff(handoff)) => {
            app.continue_handoff(&handoff, None)?;
        }
        Some(LaunchAction::ResumeSession(session)) => {
            app.resume_session(&session)?;
        }
        None => {}
    }
    Ok(())
}

fn run_in_terminal(app: &Application, ascii: bool) -> Result<Option<LaunchAction>> {
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(terminal_error)?;
    terminal.clear().map_err(terminal_error)?;
    let probe: Arc<dyn AgentProbe> = Arc::new(ProviderProbe);
    let mut events = TerminalEvents;
    let Some(state) = load_state_responsively(&mut terminal, app, ascii, &mut events, &probe)?
    else {
        return Ok(None);
    };
    let mut state = event_loop(&mut terminal, app, ascii, &mut events, &probe, state)?;
    Ok(state.launch_action.take())
}

// The event loop after the initial load. Returns the final state; its
// `launch_action` is the agent launch to run after the terminal is restored.
fn event_loop<B: Backend, E: EventSource>(
    terminal: &mut Terminal<B>,
    app: &Application,
    ascii: bool,
    events: &mut E,
    probe: &Arc<dyn AgentProbe>,
    mut state: UiState,
) -> Result<UiState> {
    let mut loader: Option<StateLoad> = None;
    let mut action: Option<Task<ActionResult>> = None;
    let mut paste_until: Option<Instant> = None;
    let mut clear_screen = false;
    loop {
        if let Some(active) = loader.as_mut() {
            match active.task.poll() {
                TaskPoll::Pending => {}
                TaskPoll::Ready(refreshed) => {
                    let current = active.generation == state.generation;
                    loader = None;
                    state.refreshing = false;
                    if current {
                        state.apply_background_refresh(refreshed);
                    } else {
                        // Read before a local change finished: load again instead of
                        // showing older state.
                        state.refresh_requested = true;
                    }
                }
                TaskPoll::Failed => {
                    loader = None;
                    state.refreshing = false;
                    clear_screen = true;
                    state.message = Some(
                        "Refresh failed: the background provider check stopped unexpectedly. Press R to retry."
                            .into(),
                    );
                }
            }
        }
        if let Some(task) = action.as_mut() {
            let result = match task.poll() {
                TaskPoll::Pending => None,
                TaskPoll::Ready(result) => Some(result),
                TaskPoll::Failed => {
                    clear_screen = true;
                    Some(Err(
                        "The action stopped unexpectedly; local state is being reloaded.".into(),
                    ))
                }
            };
            if let Some(result) = result {
                action = None;
                state.finish_action(result, Instant::now());
                // Keys pressed while the action ran were not a decision about its
                // result (for example the handoff preview a switch opens).
                if discard_queued_input(events).map_err(terminal_error)? {
                    state.interrupt(Instant::now());
                }
            }
        }
        if let Some(exit) = state.exit
            && exit_ready(exit, Instant::now(), loader.is_some(), action.is_some())
        {
            break;
        }
        if state.refresh_requested && state.exit.is_none() && loader.is_none() && action.is_none() {
            loader = Some(spawn_state_load(app, ascii, probe, state.generation)?);
            state.refresh_requested = false;
            state.refreshing = true;
        }
        if clear_screen {
            // A worker panic message may have been printed over the screen.
            terminal.clear().map_err(draw_error)?;
            clear_screen = false;
        }
        terminal
            .draw(|frame| render(frame, &state))
            .map_err(draw_error)?;
        let wait = if state.busy.is_some() || state.exit.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(250)
        };
        let Some(batch) = read_input_batch(events, wait).map_err(terminal_error)? else {
            continue;
        };
        let now = Instant::now();
        let continuing = paste_until.is_some_and(|until| now <= until);
        let (inputs, pasted) = classify_batch(batch, continuing);
        if pasted {
            paste_until = Some(now + PASTE_CONTINUATION);
        }
        for input in inputs {
            if let Some(job) = state.dispatch(app, input, now)
                && action.is_none()
            {
                state.begin_action(&job, now);
                let worker_app = app.clone();
                let cache = state.provider_cache();
                match Task::spawn("contextwake-tui-action", move || {
                    execute_action(&worker_app, job, &cache)
                }) {
                    Ok(task) => action = Some(task),
                    Err(error) => {
                        state.busy = None;
                        state.message = Some(error_text(&error));
                    }
                }
            }
        }
    }
    Ok(state)
}

// Whether the loop may stop now. A running local mutation is allowed to finish
// unless Ctrl+C was pressed twice; an in-flight provider refresh is waited for at
// most EXIT_WAIT_LIMIT, and not at all after Ctrl+C or a second Q.
fn exit_ready(exit: ExitRequest, now: Instant, loader_running: bool, action_running: bool) -> bool {
    if exit.interrupts >= 2 {
        return true;
    }
    if action_running {
        return false;
    }
    exit.interrupts >= 1
        || !loader_running
        || now.saturating_duration_since(exit.requested_at) >= EXIT_WAIT_LIMIT
}

fn load_state_responsively<B: Backend, E: EventSource>(
    terminal: &mut Terminal<B>,
    app: &Application,
    ascii: bool,
    events: &mut E,
    probe: &Arc<dyn AgentProbe>,
) -> Result<Option<UiState>> {
    let mut load = spawn_state_load(app, ascii, probe, 0)?;
    let mut frame_number = 0usize;
    let mut quit_requested_at: Option<Instant> = None;
    loop {
        match load.task.poll() {
            TaskPoll::Pending => {}
            TaskPoll::Ready(state) => return Ok(quit_requested_at.is_none().then_some(state)),
            TaskPoll::Failed if quit_requested_at.is_some() => return Ok(None),
            TaskPoll::Failed => {
                terminal.clear().map_err(draw_error)?;
                return Ok(Some(UiState::unavailable(
                    ascii,
                    "Startup inspection stopped unexpectedly.",
                )));
            }
        }
        if quit_requested_at.is_some_and(|requested| requested.elapsed() >= EXIT_WAIT_LIMIT) {
            return Ok(None);
        }
        terminal
            .draw(|frame| {
                render_loading(frame, ascii, frame_number, quit_requested_at.is_some());
            })
            .map_err(draw_error)?;
        frame_number = frame_number.wrapping_add(1);
        let Some(batch) =
            read_input_batch(events, Duration::from_millis(100)).map_err(terminal_error)?
        else {
            continue;
        };
        let (inputs, _) = classify_batch(batch, false);
        for input in inputs {
            let Input::Key(key) = input else {
                continue;
            };
            if is_interrupt(&key) {
                return Ok(None);
            }
            if matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc) {
                if quit_requested_at.is_some() {
                    return Ok(None);
                }
                quit_requested_at = Some(Instant::now());
            }
        }
    }
}

fn spinner_frame(ascii: bool, index: usize) -> &'static str {
    if ascii {
        ["|", "/", "-", "\\"][index % 4]
    } else {
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][index % 10]
    }
}

fn render_loading(
    frame: &mut ratatui::Frame<'_>,
    ascii: bool,
    frame_number: usize,
    quit_requested: bool,
) {
    let spinner = spinner_frame(ascii, frame_number);
    let area = centered(frame.area(), 62, 7);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                PRODUCT_NAME,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(if quit_requested {
                format!("{spinner} Finishing bounded probes before exit...")
            } else if ascii {
                format!("{spinner} Inspecting workspace and coding agents...")
            } else {
                format!("{spinner} Inspecting workspace and coding agents…")
            }),
            Line::from(if quit_requested {
                "Q again or Ctrl+C quits now."
            } else {
                "Q/Esc exits after active probes finish · Ctrl+C quits now."
            }),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().title(" LOADING ").borders(Borders::ALL)),
        area,
    );
}

fn render(frame: &mut ratatui::Frame<'_>, state: &UiState) {
    let area = frame.area();
    state.viewport.set(Some(area));
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_tiny(frame, state, area);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(7),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, state, rows[0]);

    let body = if area.width >= 100 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(22), Constraint::Min(40)])
            .split(rows[1]);
        render_navigation(frame, state, columns[0]);
        columns[1]
    } else {
        rows[1]
    };
    render_screen(frame, state, body);
    render_footer(frame, state, rows[2]);
    render_overlay(frame, state, area);
    render_busy(frame, state, area);
}

fn workspace_marker(state: &UiState) -> (&'static str, Color) {
    if let Some(issue) = &state.workspace_issue {
        return (issue.kind.marker(), WARNING);
    }
    match state.status.git.as_ref() {
        // A timed-out status says nothing about the tree; never present it as clean.
        Some(git) if git.timed_out => ("[GIT TIMEOUT]", WARNING),
        Some(git) if git.dirty => ("[DIRTY]", WARNING),
        _ => ("[CLEAN]", Color::Green),
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let (state_marker, marker_color) = workspace_marker(state);
    let branch = state
        .status
        .git
        .as_ref()
        .and_then(|value| value.branch.as_deref())
        .unwrap_or("no-git");
    let title = Line::from(vec![
        Span::styled(
            format!(" {PRODUCT_NAME} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{VERSION}"), Style::default().fg(MUTED)),
        Span::raw("  "),
        Span::styled(
            state.screen.title(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);
    let right = format!(
        "{} · {} {} ",
        state.status.workspace.display_name, branch, state_marker
    );
    let mut spans = title.spans;
    let used = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let padding = usize::from(area.width.saturating_sub(2))
        .saturating_sub(used)
        .saturating_sub(right.chars().count())
        .max(1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(right, Style::default().fg(marker_color)));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_navigation(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let items = Screen::NAVIGATION
        .iter()
        .map(|screen| {
            let marker = if *screen == state.screen { ">" } else { " " };
            ListItem::new(format!("{marker} {}", screen.title())).style(
                if *screen == state.screen {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(Block::default().title(" NAVIGATE ").borders(Borders::ALL)),
        area,
    );
}

fn render_screen(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    match state.screen {
        Screen::Home => render_home(frame, state, area),
        Screen::Profiles => render_profiles(frame, state, area),
        Screen::AgentCapabilities => render_agent_capabilities(frame, state, area),
        Screen::Workspaces => render_workspaces(frame, state, area),
        Screen::Sessions => render_sessions(frame, state, area),
        Screen::SessionDetail => render_session_detail(frame, state, area),
        Screen::Checkpoints => render_checkpoints(frame, state, area),
        Screen::CheckpointDetail => render_checkpoint_detail(frame, state, area),
        Screen::Handoffs => render_handoffs(frame, state, area),
        Screen::HandoffPreview => render_handoff_preview(frame, state, area),
        Screen::Usage => render_usage(frame, state, area),
        Screen::Settings => render_settings(frame, state, area),
        Screen::Diagnostics => render_diagnostics(frame, state, area),
        Screen::Onboarding => render_onboarding(frame, state, area),
        Screen::Authentication => render_authentication(frame, state, area),
        Screen::Help => render_help(frame, area),
    }
}

fn workspace_lines(state: &UiState) -> Vec<Line<'static>> {
    if let Some(issue) = &state.workspace_issue {
        let mut lines = vec![
            Line::from(Span::styled(
                format!("{} {}", issue.kind.marker(), issue.kind.summary()),
                Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "Path        {}",
                display_path(&state.status.workspace.path)
            )),
            Line::from(format!("Reason      {}", issue.reason)),
            Line::from("Recovery"),
        ];
        lines.extend(
            issue
                .recovery
                .iter()
                .map(|step| Line::from(format!("  {step}"))),
        );
        return lines;
    }
    let Some(git) = state.status.git.as_ref() else {
        return vec![
            Line::from("[NON-GIT] Ordinary directory workspace"),
            Line::from("Git-only continuity fields are unavailable."),
        ];
    };
    let repository = git
        .repository_root
        .as_ref()
        .map_or_else(|| "unavailable".into(), |path| display_path(path));
    if git.timed_out {
        return vec![
            Line::from(format!("Repository  {repository}")),
            Line::from(Span::styled(
                "State       [UNKNOWN] git status timed out",
                Style::default().fg(WARNING),
            )),
            Line::from("Changes are unknown; press R to check again."),
        ];
    }
    vec![
        Line::from(format!("Repository  {repository}")),
        Line::from(format!(
            "Branch      {}",
            git.branch.as_deref().unwrap_or("detached")
        )),
        Line::from(format!(
            "State       {}",
            if git.dirty { "[DIRTY]" } else { "[CLEAN]" }
        )),
        Line::from(format!(
            "Changes     {} staged · {} unstaged · {} untracked",
            git.staged_count, git.unstaged_count, git.untracked_count
        )),
        Line::from(format!("Diff        +{} -{}", git.additions, git.deletions)),
        Line::from(format!(
            "Last commit {}",
            git.last_commit.as_deref().unwrap_or("unavailable")
        )),
    ]
}

fn render_home(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let wide = area.width >= 78;
    let sections = Layout::default()
        .direction(if wide {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);

    let active = state
        .status
        .active_profile
        .as_ref()
        .map_or("Not configured", |profile| profile.display_name.as_str());
    let auth = state
        .status
        .active_profile
        .as_ref()
        .map_or("unknown", |profile| profile.auth_state.as_str());
    let identity = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("[ACTIVE] ", Style::default().fg(Color::Green)),
            Span::styled(active, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(format!("Agent      {}", state.status.agent.agent_id)),
        Line::from(format!(
            "Version    {}",
            state
                .status
                .agent
                .version
                .as_deref()
                .unwrap_or("unavailable")
        )),
        Line::from(format!("Auth       {auth}")),
        Line::from(""),
        Line::from(Span::styled(
            "Provider context: unavailable",
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            "No context percentage is estimated.",
            Style::default().fg(MUTED),
        )),
    ])
    .block(Block::default().title(" IDENTITY ").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(identity, sections[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(sections[1]);
    frame.render_widget(
        Paragraph::new(workspace_lines(state))
            .block(
                Block::default()
                    .title(" WORKSPACE + GIT ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        right[0],
    );

    let guardian = &state.status.continuity_guardian;
    let checkpoint_age = guardian
        .checkpoint_age_seconds
        .map_or_else(|| "none".into(), format_age);
    let risk_color = match guardian.level {
        crate::model::ContinuityRiskLevel::Current => Color::Green,
        crate::model::ContinuityRiskLevel::Notice => ACCENT,
        crate::model::ContinuityRiskLevel::Warning => WARNING,
    };
    let mut continuity = vec![Line::from(vec![
        Span::styled("LOCAL INDICATOR  ", Style::default().fg(MUTED)),
        Span::styled(
            guardian.level.as_str().to_uppercase(),
            Style::default().fg(risk_color),
        ),
    ])];
    if wide {
        continuity.extend([
            Line::from(format!("Checkpoint age   {checkpoint_age}")),
            Line::from(guardian.recommendation.clone()),
        ]);
    } else {
        continuity.push(Line::from(format!(
            "Checkpoint {checkpoint_age} · changed {}",
            guardian.observed_changed_files
        )));
    }
    let session_lines = if let Some(session) = &state.status.latest_session {
        if wide {
            vec![
                Line::from(format!("Session     {:.8}", session.id)),
                Line::from(format!("Continuity  {}", session.continuity.label())),
            ]
        } else {
            Vec::new()
        }
    } else {
        vec![Line::from(format!(
            "No session · checkpoints {} · handoffs {}",
            state.checkpoints.len(),
            state.handoffs.len()
        ))]
    };
    continuity.extend(session_lines);
    if !state.issues.is_empty() {
        continuity.push(Line::from(Span::styled(
            format!(
                "[ATTENTION] {} local issue(s) · D Diagnostics",
                state.issues.len()
            ),
            Style::default().fg(WARNING),
        )));
    }
    frame.render_widget(
        Paragraph::new(continuity)
            .block(Block::default().title(" CONTINUITY ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        right[1],
    );
}

// List state that keeps the selection visible and remembers the scroll position
// between frames.
fn scrolled_list_state(state: &UiState) -> ListState {
    ListState::default()
        .with_offset(state.list_offset.get())
        .with_selected(Some(state.selected))
}

fn selection_style() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

fn render_profiles(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.profiles.is_empty() {
        render_empty(
            frame,
            area,
            " PROFILES ",
            "No profiles.\n\n[A] Create a profile. ContextWake stores metadata only; the selected coding agent owns authentication.",
        );
        return;
    }
    let active = state.status.active_profile.as_ref().map(|value| value.id);
    let rows = state.profiles.iter().enumerate().map(|(index, profile)| {
        let selected = index == state.selected;
        let active_marker = if active == Some(profile.id) {
            "[ACTIVE]"
        } else {
            "        "
        };
        Row::new(vec![
            Cell::from(if selected { ">" } else { " " }),
            Cell::from(active_marker),
            Cell::from(profile.display_name.clone()),
            Cell::from(profile.agent_id.clone()),
            Cell::from(profile.auth_state.as_str()),
        ])
        .style(if selected {
            selection_style()
        } else {
            Style::default()
        })
    });
    let mut table_state = TableState::default()
        .with_offset(state.list_offset.get())
        .with_selected(Some(state.selected));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Length(9),
                Constraint::Percentage(45),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
        .header(
            Row::new(["", "STATE", "PROFILE", "AGENT", "AUTH"]).style(Style::default().fg(MUTED)),
        )
        .block(
            Block::default()
                .title(" PROFILES · Enter switch · A add · L auth ")
                .borders(Borders::ALL),
        ),
        area,
        &mut table_state,
    );
    state.list_offset.set(table_state.offset());
}

fn render_agent_capabilities(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.agents.is_empty() {
        render_empty(
            frame,
            area,
            " AGENT CAPABILITIES ",
            "No adapters are registered.",
        );
        return;
    }
    let sections = Layout::default()
        .direction(if area.width >= 86 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);
    let visible_agents = state.visible_agents();
    let items = visible_agents
        .iter()
        .map(|agent| {
            let install = if agent.detected_version.is_some() {
                "Installed"
            } else {
                "Not installed"
            };
            ListItem::new(format!(
                "{}\n  {} · {}",
                agent.display_name,
                install,
                agent.auth_state.as_str()
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = scrolled_list_state(state);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        " AI CODING AGENTS · {} · F filter ",
                        state.agent_filter.label()
                    ))
                    .borders(Borders::ALL),
            )
            .highlight_symbol("> ")
            .highlight_style(selection_style()),
        sections[0],
        &mut list_state,
    );
    state.list_offset.set(list_state.offset());
    let Some(agent) = visible_agents.get(state.selected).copied() else {
        frame.render_widget(
            Paragraph::new(format!(
                "No agents match the '{}' filter. Press F to change it.",
                state.agent_filter.label()
            ))
            .block(
                Block::default()
                    .title(" PROVIDER DETAILS ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
            sections[1],
        );
        return;
    };
    let capabilities = &agent.capabilities;
    let active_profile = state
        .status
        .active_profile
        .as_ref()
        .filter(|profile| profile.agent_id == agent.id);
    let mut lines = vec![
        Line::from(format!("Agent              {}", agent.display_name)),
        Line::from(format!(
            "Installed          {}",
            if agent.detected_version.is_some() {
                "Yes"
            } else {
                "No"
            }
        )),
        Line::from(format!(
            "Version            {}",
            agent.detected_version.as_deref().unwrap_or("Unavailable")
        )),
        Line::from(format!("Authentication     {}", agent.auth_state.as_str())),
        Line::from(format!(
            "Current profile    {}",
            active_profile.map_or("None", |profile| profile.display_name.as_str())
        )),
        Line::from(format!(
            "Current model      {}",
            active_profile
                .and_then(|profile| profile.model_preference.as_deref())
                .unwrap_or("Not selected")
        )),
        Line::from(""),
        capability_line("Profile isolation", &capabilities.profile_isolation),
        capability_line("Session listing", &capabilities.session_listing),
        capability_line("Native resume", &capabilities.native_resume),
        capability_line("Portable handoff", &capabilities.portable_handoff),
        capability_line("Model selection", &capabilities.model_selection),
        capability_line("Model catalog", &capabilities.available_models),
        capability_line("Structured output", &capabilities.structured_output),
        capability_line("Non-interactive", &capabilities.non_interactive_mode),
        capability_line("ACP", &capabilities.acp),
        capability_line("MCP", &capabilities.mcp),
        capability_line("Context reporting", &capabilities.context_reporting),
        capability_line("Usage reporting", &capabilities.usage_reporting),
        capability_line("Cloud handoff", &capabilities.cloud_handoff),
    ];
    if let Some(executable) = &agent.executable {
        lines.insert(
            3,
            Line::from(format!("Executable         {}", display_path(executable))),
        );
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" PROVIDER DETAILS · VERIFIED / PARTIAL / UNKNOWN ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        sections[1],
    );
}

fn capability_line<'a>(label: &'a str, capability: &'a Capability) -> Line<'a> {
    let status = format!("{:?}", capability.support).to_uppercase();
    Line::from(vec![
        Span::styled(format!("{label:<19}"), Style::default().fg(MUTED)),
        Span::styled(
            format!("{status:<13}"),
            Style::default().fg(if capability.is_available() {
                Color::Green
            } else {
                WARNING
            }),
        ),
        Span::raw(capability.detail.as_str()),
    ])
}

fn render_workspaces(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.workspaces.is_empty() {
        render_empty(
            frame,
            area,
            " WORKSPACES ",
            "No workspaces.\n\nRun: ctx workspace add <path>",
        );
        return;
    }
    let active = state.status.workspace.id;
    let items = state
        .workspaces
        .iter()
        .map(|workspace| {
            let marker = if workspace.id == active {
                "[OPEN]"
            } else {
                "      "
            };
            let missing = if state.unavailable_workspaces.contains(&workspace.id) {
                "[MISSING] "
            } else {
                ""
            };
            ListItem::new(format!(
                "{marker} {:<24} {missing}{}",
                workspace.display_name,
                display_path(&workspace.path)
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = scrolled_list_state(state);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" WORKSPACES · Enter open · repository commands never auto-run ")
                    .borders(Borders::ALL),
            )
            .highlight_style(selection_style()),
        area,
        &mut list_state,
    );
    state.list_offset.set(list_state.offset());
}

fn render_sessions(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.sessions.is_empty() {
        render_empty(
            frame,
            area,
            " RECENT SESSIONS ",
            "[NO SESSIONS]\n\nContextWake has no local session metadata.\nUse an agent's native picker, 'ctx session sync', or 'ctx session resume <id>'.",
        );
        return;
    }
    let items = state
        .sessions
        .iter()
        .map(|session| {
            ListItem::new(format!(
                "{} · {} · {} · {} · {}",
                session.title.as_deref().unwrap_or("Untitled session"),
                session
                    .provider_session_id
                    .as_deref()
                    .unwrap_or("provider id unavailable"),
                session.agent_id,
                session.continuity.label(),
                state.workspace_label(session.workspace_id)
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = scrolled_list_state(state);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" RECENT SESSIONS · Enter details · / search ")
                    .borders(Borders::ALL),
            )
            .highlight_symbol("> ")
            .highlight_style(selection_style()),
        area,
        &mut list_state,
    );
    state.list_offset.set(list_state.offset());
}

fn render_session_detail(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let Some(session) = state.sessions.get(state.selected) else {
        render_empty(
            frame,
            area,
            " SESSION DETAIL ",
            "Session metadata unavailable.",
        );
        return;
    };
    let resume_label = match session.resume_capability {
        crate::model::ResumeCapability::Native => "Native resume available for recorded profile",
        crate::model::ResumeCapability::HandoffOnly => "Workspace handoff only",
        crate::model::ResumeCapability::Unknown => "Resume capability unknown",
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Local ID       {}", session.id)),
            Line::from(format!(
                "Title          {}",
                session.title.as_deref().unwrap_or("unavailable")
            )),
            Line::from(format!(
                "Provider ID    {}",
                session
                    .provider_session_id
                    .as_deref()
                    .unwrap_or("unavailable")
            )),
            Line::from(format!("Agent          {}", session.agent_id)),
            Line::from(format!(
                "Workspace      {}",
                state.workspace_label(session.workspace_id)
            )),
            Line::from(format!(
                "Path           {}",
                state.workspace_by_id(session.workspace_id).map_or_else(
                    || "unknown".into(),
                    |workspace| display_path(&workspace.path)
                )
            )),
            Line::from(format!(
                "Model provider {}",
                session
                    .model_provider_id
                    .as_deref()
                    .unwrap_or("not recorded")
            )),
            Line::from(format!(
                "Model          {}",
                session.model.as_deref().unwrap_or("unavailable")
            )),
            Line::from(format!("Started        {}", session.started_at)),
            Line::from(format!("Last seen      {}", session.last_seen_at)),
            Line::from(format!("Continuity     {}", session.continuity.label())),
            Line::from(""),
            Line::from(Span::styled(resume_label, Style::default().fg(WARNING))),
            Line::from("ContextWake never treats a handoff-created session as a native resume."),
        ])
        .block(
            Block::default()
                .title(" SESSION DETAIL · Enter native resume · Esc back ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_checkpoints(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.checkpoints.is_empty() {
        render_empty(
            frame,
            area,
            " CHECKPOINTS ",
            "[NO CHECKPOINTS]\n\n[N] Create a deterministic project-state checkpoint.",
        );
        return;
    }
    let items = state
        .checkpoints
        .iter()
        .map(|checkpoint| {
            ListItem::new(format!(
                "{} · {} · {} · {}",
                checkpoint.created_at.format("%Y-%m-%d %H:%M"),
                state.workspace_label(checkpoint.workspace_id),
                checkpoint.redaction_status.as_str(),
                checkpoint.objective
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = scrolled_list_state(state);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" CHECKPOINTS · N create ")
                    .borders(Borders::ALL),
            )
            .highlight_symbol("> ")
            .highlight_style(selection_style()),
        area,
        &mut list_state,
    );
    state.list_offset.set(list_state.offset());
}

fn render_checkpoint_detail(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let Some(checkpoint) = state.checkpoints.get(state.selected) else {
        render_empty(
            frame,
            area,
            " CHECKPOINT DETAIL ",
            "No checkpoint selected.",
        );
        return;
    };
    let git = checkpoint.git.as_ref();
    let lines = vec![
        Line::from(format!("Checkpoint      {}", checkpoint.id)),
        Line::from(format!("Created         {}", checkpoint.created_at)),
        Line::from(format!(
            "Workspace       {}",
            state.workspace_label(checkpoint.workspace_id)
        )),
        Line::from(format!(
            "Path            {}",
            state.workspace_by_id(checkpoint.workspace_id).map_or_else(
                || "unknown".into(),
                |workspace| display_path(&workspace.path)
            )
        )),
        Line::from(format!(
            "Agent           {}",
            checkpoint.agent_id.as_deref().unwrap_or("not recorded")
        )),
        Line::from(format!(
            "Model backend   {}",
            checkpoint
                .model_provider_id
                .as_deref()
                .unwrap_or("not recorded")
        )),
        Line::from(format!(
            "Model           {}",
            checkpoint.model.as_deref().unwrap_or("not recorded")
        )),
        Line::from(format!("Objective       {}", checkpoint.objective)),
        Line::from(format!(
            "Current task    {}",
            checkpoint.active_task.as_deref().unwrap_or("not recorded")
        )),
        Line::from(format!(
            "Files changed   {}",
            checkpoint.files_changed.len()
        )),
        Line::from(format!(
            "Git             {} / +{} -{}",
            git.and_then(|snapshot| snapshot.branch.as_deref())
                .unwrap_or("non-git or detached"),
            git.map_or(0, |snapshot| snapshot.additions),
            git.map_or(0, |snapshot| snapshot.deletions)
        )),
        Line::from(format!(
            "Validation      {} result(s)",
            checkpoint.validation_results.len()
        )),
        Line::from(format!(
            "Redaction       {}",
            checkpoint.redaction_status.as_str()
        )),
        Line::from(""),
        Line::from("Accessible project state only; hidden reasoning is never captured."),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" CHECKPOINT DETAIL - Esc back ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_handoffs(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    if state.handoffs.is_empty() {
        render_empty(
            frame,
            area,
            " HANDOFFS ",
            "[NO HANDOFFS]\n\nCreate a checkpoint, then press [N] here.",
        );
        return;
    }
    let items = state
        .handoffs
        .iter()
        .map(|handoff| {
            ListItem::new(format!(
                "{} · {} · schema {} · checkpoint {}",
                handoff.created_at.format("%Y-%m-%d %H:%M"),
                state.handoff_workspace_label(handoff),
                handoff.schema_version,
                handoff.checkpoint_id
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = scrolled_list_state(state);
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(" HANDOFFS · N create from latest checkpoint · Enter preview ")
                    .borders(Borders::ALL),
            )
            .highlight_symbol("> ")
            .highlight_style(selection_style()),
        area,
        &mut list_state,
    );
    state.list_offset.set(list_state.offset());
}

fn render_handoff_preview(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let Some(handoff) = state.handoffs.get(state.selected) else {
        render_empty(frame, area, " HANDOFF PREVIEW ", "Handoff unavailable.");
        return;
    };
    let checkpoint = state
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.id == handoff.checkpoint_id);
    let target = state.handoff_workspace(handoff);
    let launch_instruction = if handoff.checkpoint_id.is_nil() {
        format!(
            "Imported package: use `ctx handoff continue {} --workspace <path>`",
            handoff.id
        )
    } else {
        "[Enter] Start a NEW agent session from this handoff".into()
    };
    let launch_as = state.status.active_profile.as_ref().map_or_else(
        || "no active profile".into(),
        |profile| format!("{} ({})", profile.display_name, profile.agent_id),
    );
    let mut lines = vec![
        Line::from(Span::styled(
            "[INSPECT BEFORE EXPORT]",
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Handoff      {}", handoff.id)),
        Line::from(format!("Schema       {}", handoff.schema_version)),
        Line::from(format!("Integrity    {}", handoff.sha256)),
        Line::from(format!(
            "Workspace    {}",
            state.handoff_workspace_label(handoff)
        )),
        Line::from(format!(
            "Path         {}",
            target.map_or_else(
                || "unknown".into(),
                |workspace| display_path(&workspace.path)
            )
        )),
        Line::from(format!(
            "Branch       {}",
            checkpoint
                .and_then(|checkpoint| checkpoint.git.as_ref())
                .and_then(|git| git.branch.as_deref())
                .unwrap_or("non-git or detached")
        )),
        Line::from(format!("Launch as    {launch_as}")),
        Line::from(""),
        Line::from("This package contains explicit project state only."),
        Line::from("Commands are untrusted notes and are never auto-executed."),
        Line::from(""),
    ];
    if let Some(workspace) = target
        && workspace.id != state.status.workspace.id
    {
        lines.push(Line::from(Span::styled(
            format!(
                "[OTHER WORKSPACE] The agent starts in {}, not in {}.",
                workspace.display_name, state.status.workspace.display_name
            ),
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        )));
    }
    lines.extend([
        Line::from(Span::styled(
            launch_instruction,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("[Esc] Return without launching a provider"),
    ]);
    if let Some(checkpoint) = checkpoint {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Objective",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(checkpoint.objective.clone()));
        lines.push(Line::from(format!(
            "Files modified: {} · redaction: {}",
            checkpoint.files_changed.len(),
            checkpoint.redaction_status.as_str()
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" HANDOFF PREVIEW · Enter new session · Esc back ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_usage(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let columns = Layout::default()
        .direction(if area.width >= 72 {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "[UNAVAILABLE]",
                Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(state.usage.provider_usage_message.clone()),
            Line::from(""),
            Line::from("No quota, credits, or remaining-token values are estimated."),
        ])
        .block(
            Block::default()
                .title(" PROVIDER-REPORTED USAGE ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true }),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "Sessions         {}",
                state.usage.local_session_count
            )),
            Line::from(format!(
                "Profile switches {}",
                state.usage.local_profile_switch_count
            )),
            Line::from(format!(
                "Checkpoints      {}",
                state.usage.local_checkpoint_count
            )),
            Line::from(format!(
                "Handoffs         {}",
                state.usage.local_handoff_count
            )),
            Line::from(""),
            Line::from(Span::styled(
                "[LOCAL ONLY] Telemetry is disabled by default.",
                Style::default().fg(Color::Green),
            )),
        ])
        .block(
            Block::default()
                .title(" LOCAL ACTIVITY ")
                .borders(Borders::ALL),
        ),
        columns[1],
    );
}

fn render_settings(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Configuration       {}", "run `ctx config path`")),
            Line::from(format!("Telemetry           {}", false)),
            Line::from("Agent adapters       Compile-time registry only"),
            Line::from(format!("ASCII mode          {}", state.ascii)),
            Line::from(""),
            Line::from("Configuration precedence"),
            Line::from("CLI > environment > trusted project > profile > global > defaults"),
            Line::from(""),
            Line::from("Repository validation commands are argv arrays and never auto-run."),
        ])
        .block(
            Block::default()
                .title(" SETTINGS · edit with ctx config path ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_diagnostics(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let warning_row = |name: &'static str, summary: String| {
        Row::new(vec![
            Cell::from("WARN").style(Style::default().fg(WARNING)),
            Cell::from(name),
            Cell::from(summary),
        ])
    };
    let mut rows = state
        .doctor
        .checks
        .iter()
        .map(|check| {
            let (label, color) = match check.status {
                crate::doctor::CheckStatus::Pass => ("PASS", Color::Green),
                crate::doctor::CheckStatus::Warning => ("WARN", WARNING),
                crate::doctor::CheckStatus::Fail => ("FAIL", Color::Red),
            };
            Row::new(vec![
                Cell::from(label).style(Style::default().fg(color)),
                Cell::from(check.name.clone()),
                Cell::from(check.summary.clone()),
            ])
        })
        .collect::<Vec<_>>();
    if let Some(issue) = &state.workspace_issue {
        rows.push(warning_row(
            "Workspace",
            format!("{} {}", issue.kind.marker(), issue.reason),
        ));
    }
    rows.extend(
        state
            .issues
            .iter()
            .map(|issue| warning_row("Local state", issue.clone())),
    );
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(6),
                Constraint::Length(22),
                Constraint::Min(20),
            ],
        )
        .block(
            Block::default()
                .title(" DIAGNOSTICS · R refresh · secrets never displayed ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_onboarding(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Welcome to {PRODUCT_NAME}"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Profiles are local labels; isolation follows each adapter's evidence."),
        Line::from("Credentials remain provider-owned and are never copied by ContextWake."),
        Line::from(""),
    ];
    for agent in &state.agents {
        lines.push(Line::from(format!(
            "[{}] {:<18} {}",
            if agent.detected_version.is_some() {
                "DETECTED"
            } else {
                "NOT FOUND"
            },
            agent.display_name,
            agent.detected_version.as_deref().unwrap_or("")
        )));
    }
    lines.push(Line::from(
        "[Enter] Personal profile  [A] Custom profile  [Q] Quit",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .block(Block::default().title(" FIRST RUN ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        centered(area, 76, 19),
    );
}

fn render_authentication(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let profile = state.profiles.get(state.selected);
    let login_detail = profile
        .and_then(|profile| {
            state
                .agents
                .iter()
                .find(|agent| agent.id == profile.agent_id)
        })
        .map_or(
            "Use the selected coding agent's provider-owned login flow.",
            |agent| agent.capabilities.login.detail.as_str(),
        );
    let command = profile.map_or_else(
        || "ctx profile add Personal".into(),
        |profile| format!("ctx profile login {}", profile.name),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "PROVIDER-OWNED AUTHENTICATION",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("ContextWake does not render password or API-key fields."),
            Line::from("Leave the TUI and run:"),
            Line::from(""),
            Line::from(Span::styled(command, Style::default().fg(Color::Green))),
            Line::from(""),
            Line::from(login_detail),
            Line::from("[Esc] Back"),
        ])
        .block(
            Block::default()
                .title(" AUTHENTICATION ")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true }),
        centered(area, 70, 14),
    );
}

fn render_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("G  Home          P  Profiles       I  Agent capabilities"),
            Line::from("W  Workspaces    S  Sessions       C  Checkpoints"),
            Line::from("O  Handoffs      U  Usage          T  Settings"),
            Line::from("D  Diagnostics"),
            Line::from(""),
            Line::from("J/K or arrows    Navigate"),
            Line::from("Enter            Open / confirm"),
            Line::from("/                Search local sessions"),
            Line::from("R                Refresh"),
            Line::from("Esc              Back / cancel"),
            Line::from("Q                Quit"),
            Line::from("Ctrl+C           Quit without waiting or launching"),
            Line::from(""),
            Line::from("Profile switch confirmation"),
            Line::from("Enter  Create checkpoint + handoff, then switch"),
            Line::from("X      Switch without continuity artifact"),
        ])
        .block(
            Block::default()
                .title(" KEYBOARD REFERENCE ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_footer(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let text = state.message.as_deref().unwrap_or(
        "P Profiles  W Workspaces  S Sessions  C Checkpoints  O Handoffs  ? Help  Q Quit",
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(if state.message.is_some() {
                ACCENT
            } else {
                MUTED
            }))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_overlay(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    match &state.input {
        InputMode::Normal => {}
        InputMode::AddProfile { value, agent_index } => {
            let popup = centered(area, 62, 10);
            let agent = state.agents.get(*agent_index).map_or_else(
                || "unavailable".to_string(),
                |agent| {
                    if agent.detected_version.is_some() {
                        agent.display_name.clone()
                    } else {
                        format!("{} [NOT INSTALLED]", agent.display_name)
                    }
                },
            );
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("Create a local profile label"),
                    Line::from(format!("Agent: {agent}  [Tab] change")),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("> {value}"),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Enter create · Esc cancel · provider auth remains separate"),
                ])
                .block(
                    Block::default()
                        .title(" NEW PROFILE ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(ACCENT)),
                ),
                popup,
            );
        }
        InputMode::CreateCheckpoint { value } => {
            let popup = centered(area, 72, 9);
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("What is the current objective?"),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("> {value}"),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Enter create deterministic checkpoint · Esc cancel"),
                ])
                .block(
                    Block::default()
                        .title(" CREATE CHECKPOINT ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(ACCENT)),
                )
                .wrap(Wrap { trim: true }),
                popup,
            );
        }
        InputMode::SearchSessions { value } => {
            let popup = centered(area, 66, 9);
            frame.render_widget(Clear, popup);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("Search local session IDs, provider, or model"),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("> {value}"),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Enter apply · Esc cancel · R clears after search"),
                ])
                .block(
                    Block::default()
                        .title(" SESSION SEARCH ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(ACCENT)),
                ),
                popup,
            );
        }
        InputMode::ConfirmSwitch { target } => {
            if let Some(profile) = state.profiles.iter().find(|profile| profile.id == *target) {
                render_confirm_switch(frame, state, profile, area);
            }
        }
    }
}

fn render_confirm_switch(
    frame: &mut ratatui::Frame<'_>,
    state: &UiState,
    profile: &Profile,
    area: Rect,
) {
    let popup = centered(area, 76, 15);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" CONFIRM IDENTITY CHANGE ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(WARNING));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    // The answer keys get their own rows at the bottom, so they stay visible even
    // when the details above them are clipped on a small terminal.
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(inner);
    let changes = match (&state.workspace_issue, state.status.git.as_ref()) {
        (Some(issue), _) => format!("{} workspace unavailable", issue.kind.marker()),
        (None, Some(git)) if git.timed_out => "unknown (git status timed out)".into(),
        (None, Some(git)) => format!(
            "{} staged · {} unstaged · {} untracked · +{} -{}",
            git.staged_count, git.unstaged_count, git.untracked_count, git.additions, git.deletions
        ),
        (None, None) => "Non-Git workspace".into(),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(
                    "Switch {} → {}",
                    state
                        .status
                        .active_profile
                        .as_ref()
                        .map_or("No profile", |value| value.display_name.as_str()),
                    profile.display_name
                ),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!(
                "Workspace  {}",
                state.status.workspace.display_name
            )),
            Line::from(format!("Git        {changes}")),
            Line::from(""),
            Line::from(Span::styled(
                "Native cross-profile session resume is not assumed.",
                Style::default().fg(WARNING),
            )),
        ])
        .wrap(Wrap { trim: true }),
        sections[0],
    );
    let compact = inner.width < 48;
    let enter = match (&state.workspace_issue, compact) {
        (Some(issue), false) => {
            format!("[Enter] Unavailable: workspace is {}", issue.kind.marker())
        }
        (Some(issue), true) => format!("[Enter] Unavailable ({})", issue.kind.marker()),
        (None, false) => "[Enter] Create checkpoint + handoff, then switch".into(),
        (None, true) => "[Enter] Checkpoint + handoff, switch".into(),
    };
    let without_artifact = if compact {
        "[X]     Switch, no artifact"
    } else {
        "[X]     Switch without a continuity artifact"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(enter),
            Line::from(without_artifact),
            Line::from("[Esc]   Cancel"),
        ]),
        sections[1],
    );
}

fn render_busy(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let Some(busy) = &state.busy else {
        return;
    };
    let tick = usize::try_from(busy.started_at.elapsed().as_millis() / 100).unwrap_or(0);
    let popup = centered(area, 66, 7);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{} {}", spinner_frame(state.ascii, tick), busy.label),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Input is paused until this finishes."),
            Line::from("Ctrl+C quits once it is done."),
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(" WORKING ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT)),
        ),
        popup,
    );
}

fn render_empty(frame: &mut ratatui::Frame<'_>, area: Rect, title: &str, message: &str) {
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::default().title(title).borders(Borders::ALL))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_tiny(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let profile = state
        .status
        .active_profile
        .as_ref()
        .map_or("none", |value| value.display_name.as_str());
    let git = match state.status.git.as_ref() {
        Some(value) if value.timed_out => "unknown",
        Some(value) if value.dirty => "dirty",
        Some(_) => "clean",
        None => "no-git",
    };
    let workspace = state.workspace_issue.as_ref().map_or_else(
        || state.status.workspace.display_name.clone(),
        |issue| {
            format!(
                "{} {}",
                state.status.workspace.display_name,
                issue.kind.marker()
            )
        },
    );
    // A modal stays open while the terminal is too small, but its keys are paused
    // (see handle_tiny_input), so it is named here rather than left invisible.
    let paused = match (&state.busy, state.input.title()) {
        (Some(busy), _) => Some(format!("Working: {}", busy.label)),
        (None, Some(title)) => Some(format!("Paused: {title} · Esc cancels")),
        (None, None) => None,
    };
    let mut lines = vec![Line::from(Span::styled(
        PRODUCT_NAME,
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    if let Some(paused) = &paused {
        lines.push(Line::from(Span::styled(
            paused.clone(),
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(message) = &state.message {
        lines.push(Line::from(Span::styled(
            message.clone(),
            Style::default().fg(ACCENT),
        )));
    }
    lines.extend([
        Line::from(format!("Profile: {profile}")),
        Line::from(format!("Workspace: {workspace}")),
        Line::from(format!("Git: {git}")),
        Line::from("Terminal is narrow."),
        Line::from(if paused.is_some() {
            format!("Enlarge to {MIN_WIDTH}x{MIN_HEIGHT} to continue")
        } else {
            format!("Enlarge to {MIN_WIDTH}x{MIN_HEIGHT} · Q quit")
        }),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(2));
    let height = max_height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60)
    } else {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3_600)
    }
}

fn terminal_error(error: io::Error) -> ContextWakeError {
    ContextWakeError::InvalidData(format!("terminal operation failed: {error}"))
}

fn draw_error(error: impl std::fmt::Display) -> ContextWakeError {
    ContextWakeError::InvalidData(format!("terminal draw failed: {error}"))
}

// Paths are shown in their conventional form (no Windows `\\?\` prefix) and
// without terminal control characters.
fn display_path(path: &Path) -> String {
    sanitize_terminal(&simplify_path(path).display().to_string())
}

// One terminal-safe, redacted line describing an error for the footer.
fn error_text(error: &ContextWakeError) -> String {
    let text = error.to_string();
    if text.contains("UNIQUE constraint failed: profiles.name") {
        return "a profile with this name already exists".into();
    }
    one_line(&text, 300)
}

fn validation_text(error: &ContextWakeError) -> String {
    match error {
        ContextWakeError::InvalidData(message) | ContextWakeError::Configuration(message) => {
            one_line(message, 200)
        }
        other => error_text(other),
    }
}

fn one_line(text: &str, max_characters: usize) -> String {
    let safe = SecretScanner::new().redact(&sanitize_terminal(text)).text;
    let compact = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_characters {
        return compact;
    }
    let mut shortened = compact
        .chars()
        .take(max_characters.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

fn session_matches(session: &Session, query: &str) -> bool {
    session.id.to_string().to_lowercase().contains(query)
        || session
            .title
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
        || session
            .provider_session_id
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
        || session.agent_id.to_lowercase().contains(query)
        || session
            .model
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::AppConfig;
    use crate::doctor::{CheckStatus, DoctorCheck};
    use crate::git::GitClient;
    use crate::model::{GitSnapshot, RedactionStatus};
    use crate::paths::AppPaths;
    use crate::provider::{AgentAdapter, AgentRegistry, CodexAdapter};
    use crate::store::Store;

    fn test_workspace(path: PathBuf, display_name: &str) -> Workspace {
        let now = Utc::now();
        Workspace {
            id: Uuid::new_v4(),
            git_root: Some(path.clone()),
            path,
            display_name: display_name.into(),
            trust_state: TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            created_at: now,
            last_opened_at: now,
        }
    }

    fn test_profile(
        name: &str,
        display_name: &str,
        agent_home: impl Into<PathBuf>,
        auth_state: AuthState,
    ) -> Profile {
        let now = Utc::now();
        Profile {
            id: Uuid::new_v4(),
            name: name.into(),
            display_name: display_name.into(),
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model_preference: None,
            description: None,
            agent_home: agent_home.into(),
            account_fingerprint: None,
            auth_state,
            created_at: now,
            updated_at: now,
            last_used_at: Some(now),
        }
    }

    fn test_checkpoint(workspace_id: Uuid, objective: &str) -> Checkpoint {
        Checkpoint {
            schema_version: 2,
            id: Uuid::new_v4(),
            workspace_id,
            session_id: None,
            agent_id: Some("codex".into()),
            model_provider_id: Some("openai".into()),
            model: None,
            profile_id: None,
            objective: objective.into(),
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
            created_at: Utc::now(),
            redaction_status: RedactionStatus::Clean,
        }
    }

    fn test_session(workspace_id: Uuid, title: &str, agent_id: &str) -> Session {
        let now = Utc::now();
        Session {
            id: Uuid::new_v4(),
            provider_session_id: Some(format!("provider-{title}")),
            title: Some(title.into()),
            workspace_id,
            profile_id: None,
            agent_id: agent_id.into(),
            model_provider_id: Some("openai".into()),
            model: None,
            started_at: now,
            last_seen_at: now,
            resume_capability: crate::model::ResumeCapability::Native,
            archived: false,
            continuity: crate::model::ContinuityKind::NativeResume,
        }
    }

    fn test_handoff(checkpoint_id: Uuid, schema_version: &str) -> HandoffRecord {
        HandoffRecord {
            id: Uuid::new_v4(),
            checkpoint_id,
            schema_version: schema_version.into(),
            storage_path: PathBuf::from("C:/state/handoffs/example"),
            sha256: "0".repeat(64),
            created_at: Utc::now(),
        }
    }

    fn fixture() -> UiState {
        let workspace = test_workspace(
            PathBuf::from("C:/very/long/workspace/path/example"),
            "Example Workspace",
        );
        let profile = test_profile("work", "Work", "C:/state/codex/work", AuthState::SignedIn);
        let local = LocalSnapshot {
            status: StatusView {
                application: PRODUCT_NAME.into(),
                version: VERSION.into(),
                active_profile: Some(profile.clone()),
                workspace: workspace.clone(),
                git: Some(GitSnapshot {
                    repository_root: workspace.git_root.clone(),
                    branch: Some("feature/continuity".into()),
                    dirty: true,
                    staged_count: 2,
                    unstaged_count: 3,
                    untracked_count: 1,
                    additions: 120,
                    deletions: 31,
                    last_commit: Some("Implement checkpoint schema".into()),
                    ..GitSnapshot::default()
                }),
                agent: AgentHealth {
                    agent_id: "codex".into(),
                    installed: true,
                    executable: Some(PathBuf::from("codex")),
                    version: Some("codex-cli 0.153.4".into()),
                    auth_state: AuthState::SignedIn,
                    message: "detected".into(),
                },
                latest_session: None,
                context_information: "unavailable".into(),
                continuity_guardian: crate::model::ContinuityRiskStatus {
                    level: crate::model::ContinuityRiskLevel::Notice,
                    latest_checkpoint_id: None,
                    checkpoint_age_seconds: None,
                    workspace_changed: true,
                    observed_changed_files: 6,
                    observed_diff_lines: 151,
                    recommendation: "Create a checkpoint before switching profiles.".into(),
                    basis: "LOCAL INDICATOR".into(),
                },
            },
            workspace_issue: None,
            profiles: vec![profile.clone()],
            checkpoints: vec![test_checkpoint(
                workspace.id,
                "Continue context continuity implementation",
            )],
            workspaces: vec![workspace],
            unavailable_workspaces: BTreeSet::new(),
            sessions: Vec::new(),
            handoffs: Vec::new(),
            usage: UsageSummary {
                provider_usage_available: false,
                provider_usage_message: "Unavailable".into(),
                local_session_count: 0,
                local_checkpoint_count: 1,
                local_handoff_count: 0,
                local_profile_switch_count: 1,
            },
            issues: Vec::new(),
        };
        let agents = vec![CodingAgent {
            id: "codex".into(),
            display_name: "OpenAI Codex CLI".into(),
            adapter_version: VERSION.into(),
            detected_version: Some("codex-cli 0.154.0".into()),
            executable: Some(PathBuf::from("codex")),
            auth_state: crate::model::AuthState::SignedIn,
            capabilities: CodexAdapter::with_executable("codex").capabilities(),
        }];
        let doctor = DoctorReport {
            application: PRODUCT_NAME.into(),
            version: VERSION.into(),
            checks: vec![DoctorCheck {
                name: "Application".into(),
                status: CheckStatus::Pass,
                summary: "ok".into(),
                action: None,
            }],
        };
        UiState::from_parts(
            local,
            agents,
            Some(ProbeContext::of(&profile)),
            doctor,
            false,
        )
    }

    // An application over a private temporary state root. Its agent registry is
    // never probed: tests pass a fake AgentProbe.
    fn test_app(root: &Path) -> Application {
        let paths = AppPaths::from_root(root.join("home"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        Application {
            paths,
            config: AppConfig::default(),
            store,
            git: GitClient::new(Duration::from_secs(10)),
            agents: AgentRegistry::discover(),
        }
    }

    struct NoAgents;

    impl AgentProbe for NoAgents {
        fn detect(
            &self,
            app: &Application,
            _active: Option<(&str, &Path)>,
        ) -> Vec<Result<AgentHealth>> {
            app.agents
                .implemented()
                .map(|adapter| {
                    Ok(AgentHealth {
                        agent_id: adapter.id().into(),
                        installed: false,
                        executable: None,
                        version: None,
                        auth_state: AuthState::Unknown,
                        message: "not probed in tests".into(),
                    })
                })
                .collect()
        }
    }

    // A probe that hangs like one whose descendant holds the pipes, until released.
    // It owns the temporary state root so the directory outlives the workers.
    struct StuckProbe {
        release: Mutex<mpsc::Receiver<()>>,
        _root: tempfile::TempDir,
    }

    impl AgentProbe for StuckProbe {
        fn detect(
            &self,
            app: &Application,
            active: Option<(&str, &Path)>,
        ) -> Vec<Result<AgentHealth>> {
            if let Ok(release) = self.release.lock() {
                let _ = release.recv();
            }
            NoAgents.detect(app, active)
        }
    }

    // Terminal input in groups: one group is what a user typed at one moment, so it
    // is read as one batch; the next group arrives only on a later, blocking poll.
    struct ScriptedEvents {
        groups: VecDeque<VecDeque<Event>>,
        current: VecDeque<Event>,
        idle_polls: usize,
    }

    impl ScriptedEvents {
        fn new(groups: Vec<Vec<Event>>) -> Self {
            Self {
                groups: groups.into_iter().map(VecDeque::from).collect(),
                current: VecDeque::new(),
                idle_polls: 0,
            }
        }
    }

    impl EventSource for ScriptedEvents {
        fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
            if !self.current.is_empty() {
                return Ok(true);
            }
            if timeout.is_zero() {
                return Ok(false);
            }
            if let Some(next) = self.groups.pop_front() {
                self.current = next;
                return Ok(!self.current.is_empty());
            }
            self.idle_polls += 1;
            assert!(
                self.idle_polls < 4_000,
                "the TUI did not exit after its scripted input"
            );
            thread::sleep(Duration::from_millis(5));
            Ok(false)
        }

        fn read(&mut self) -> io::Result<Event> {
            self.current
                .pop_front()
                .ok_or_else(|| io::Error::other("no scripted event is ready"))
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(key(code))
    }

    fn release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn send(state: &mut UiState, app: &Application, code: KeyCode) -> Option<Action> {
        state.dispatch(app, Input::Key(key(code)), Instant::now())
    }

    fn draw(state: &UiState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| render(frame, state)).expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn active_workspace_id(app: &Application) -> Option<Uuid> {
        app.store
            .active_workspace()
            .expect("active workspace")
            .map(|workspace| workspace.id)
    }

    fn workspace_dir(root: &Path, name: &str) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).expect("workspace directory");
        directory
    }

    #[test]
    fn renders_wide_dashboard() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = fixture();
        terminal.draw(|frame| render(frame, &state)).expect("draw");
        let content = terminal.backend().buffer().content();
        assert!(content.iter().any(|cell| cell.symbol() == "A"));
    }

    #[test]
    fn loading_state_renders_before_provider_probes() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_loading(frame, false, 0, false))
            .expect("draw");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("LOADING"));
        assert!(rendered.contains("coding agents"));
        assert!(rendered.contains("Q/Esc"));
    }

    #[test]
    fn background_refresh_preserves_navigation_and_queued_follow_up() {
        let mut current = fixture();
        current.screen = Screen::AgentCapabilities;
        current.agent_filter = AgentFilter::Installed;
        current.refresh_requested = true;
        current.refreshing = true;
        current.message = Some("Created a checkpoint.".into());

        let mut refreshed = fixture();
        refreshed.agents[0].detected_version = Some("codex-cli 9.9.9".into());
        current.apply_background_refresh(refreshed);

        assert_eq!(current.screen, Screen::AgentCapabilities);
        assert_eq!(current.agent_filter, AgentFilter::Installed);
        assert_eq!(
            current.agents[0].detected_version.as_deref(),
            Some("codex-cli 9.9.9")
        );
        assert!(current.refresh_requested);
        assert!(!current.refreshing);
        assert_eq!(current.message.as_deref(), Some("Created a checkpoint."));
    }

    #[test]
    fn no_profile_health_is_agent_neutral() {
        let health = unconfigured_agent_health();
        assert_eq!(health.agent_id, "unconfigured");
        assert!(!health.installed);
        assert_eq!(health.auth_state, AuthState::Unknown);
    }

    #[test]
    fn renders_narrow_dashboard_without_panicking() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = fixture();
        terminal.draw(|frame| render(frame, &state)).expect("draw");
    }

    #[test]
    fn onboarding_fits_nine_agents_and_actions_in_a_standard_terminal() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = fixture();
        state.screen = Screen::Onboarding;
        let template = state.agents[0].clone();
        state.agents = [
            ("codex", "Codex"),
            ("claude", "Claude Code"),
            ("github-copilot", "GitHub Copilot"),
            ("cursor", "Cursor"),
            ("opencode", "OpenCode"),
            ("gemini", "Gemini"),
            ("kiro", "Kiro"),
            ("kimi", "Kimi Code"),
            ("grok", "Grok Build"),
        ]
        .into_iter()
        .map(|(id, display_name)| CodingAgent {
            id: id.into(),
            display_name: display_name.into(),
            ..template.clone()
        })
        .collect();

        terminal.draw(|frame| render(frame, &state)).expect("draw");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("Grok Build"), "{rendered}");
        assert!(rendered.contains("Personal profile"), "{rendered}");
        assert!(rendered.contains("Custom profile"), "{rendered}");
    }

    #[test]
    fn renders_tiny_terminal_fallback() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let state = fixture();
        terminal.draw(|frame| render(frame, &state)).expect("draw");
    }

    #[test]
    fn agent_capability_screen_renders_real_adapter_contract() {
        let backend = TestBackend::new(110, 34);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = fixture();
        state.screen = Screen::AgentCapabilities;
        terminal.draw(|frame| render(frame, &state)).expect("draw");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("Native resume"));
        assert!(rendered.contains("OpenAI Codex CLI"));
    }

    #[test]
    fn agent_filters_use_install_auth_resume_and_acp_capabilities() {
        let mut state = fixture();
        let installed = &state.agents[0];
        assert!(AgentFilter::Installed.includes(installed));
        assert!(AgentFilter::Authenticated.includes(installed));
        assert!(AgentFilter::NativeResume.includes(installed));
        assert!(!AgentFilter::Acp.includes(installed));

        let mut absent = installed.clone();
        absent.detected_version = None;
        absent.auth_state = AuthState::Unknown;
        state.agents.push(absent);
        state.agent_filter = AgentFilter::Installed;
        state.screen = Screen::AgentCapabilities;
        assert_eq!(state.current_len(), 1);
        assert_eq!(AgentFilter::Acp.next(), AgentFilter::All);
    }

    #[test]
    fn every_scaffolded_screen_renders_with_empty_provider_data() {
        let screens = [
            Screen::Home,
            Screen::Profiles,
            Screen::AgentCapabilities,
            Screen::Workspaces,
            Screen::Sessions,
            Screen::SessionDetail,
            Screen::Checkpoints,
            Screen::CheckpointDetail,
            Screen::Handoffs,
            Screen::HandoffPreview,
            Screen::Usage,
            Screen::Settings,
            Screen::Diagnostics,
            Screen::Onboarding,
            Screen::Authentication,
            Screen::Help,
        ];
        for screen in screens {
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut state = fixture();
            state.screen = screen;
            state.status.agent.installed = false;
            state.status.agent.version = None;
            state.status.agent.auth_state = AuthState::Unknown;
            state.status.agent.message = "agent unavailable".into();
            terminal
                .draw(|frame| render(frame, &state))
                .unwrap_or_else(|error| panic!("{screen:?} failed to render: {error}"));
        }
    }

    #[test]
    fn unicode_workspace_and_ascii_mode_both_render() {
        for ascii in [false, true] {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut state = fixture();
            state.ascii = ascii;
            state.status.workspace.display_name = "工具 · continuity".into();
            terminal.draw(|frame| render(frame, &state)).expect("draw");
        }
    }

    #[test]
    fn handoff_preview_requires_explicit_launch_action() {
        let mut state = fixture();
        let checkpoint_id = state.checkpoints[0].id;
        let handoff_id = Uuid::new_v4();
        state.screen = Screen::HandoffPreview;
        state.handoffs.push(HandoffRecord {
            id: handoff_id,
            checkpoint_id,
            schema_version: "1.1.0".into(),
            storage_path: PathBuf::from("C:/state/handoffs/example"),
            sha256: "0".repeat(64),
            created_at: Utc::now(),
        });

        assert!(state.launch_action.is_none());
        assert!(state.queue_selected_handoff());
        let expected = handoff_id.to_string();
        assert_eq!(
            state.launch_action,
            Some(LaunchAction::ContinueHandoff(expected))
        );

        state.launch_action = None;
        state.handoffs[0].checkpoint_id = Uuid::nil();
        assert!(!state.queue_selected_handoff());
        assert!(state.launch_action.is_none());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("--workspace"))
        );
    }

    #[test]
    fn formats_checkpoint_age_compactly() {
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(125), "2m");
        assert_eq!(format_age(7_500), "2h 5m");
        assert_eq!(format_age(90_000), "1d 1h");
    }

    #[test]
    fn session_search_uses_available_local_metadata() {
        let mut state = fixture();
        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4(),
            provider_session_id: Some("feature-inspection".into()),
            title: Some("Inspection workflow".into()),
            // Sessions always belong to a known workspace (a foreign key in the store);
            // native resume is refused for one the TUI cannot show (tui#6).
            workspace_id: state.status.workspace.id,
            profile_id: None,
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model: Some("gpt-test".into()),
            started_at: now,
            last_seen_at: now,
            resume_capability: crate::model::ResumeCapability::Native,
            archived: false,
            continuity: crate::model::ContinuityKind::NativeResume,
        };
        assert!(session_matches(&session, "inspection"));
        assert!(session_matches(&session, "workflow"));
        assert!(session_matches(&session, "gpt-test"));
        assert!(!session_matches(&session, "unrelated"));

        state.screen = Screen::SessionDetail;
        state.sessions.push(session.clone());
        assert!(state.queue_selected_session());
        assert_eq!(
            state.launch_action,
            Some(LaunchAction::ResumeSession(session.id.to_string()))
        );

        state.launch_action = None;
        state.sessions[0].resume_capability = crate::model::ResumeCapability::HandoffOnly;
        assert!(!state.queue_selected_session());
        assert!(state.launch_action.is_none());
    }

    // tui#1: a stale active workspace degrades into an on-screen issue.
    #[test]
    fn missing_active_workspace_degrades_instead_of_exiting() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let directory = workspace_dir(root.path(), "demo clone");
        let workspace = app
            .register_workspace(&directory, TrustState::Untrusted, true)
            .expect("register");
        std::fs::remove_dir_all(&directory).expect("delete workspace");

        let state = UiState::load(&app, false, &NoAgents);

        assert_eq!(state.status.workspace.id, workspace.id);
        let issue = state
            .workspace_issue
            .as_ref()
            .expect("the stale workspace is reported");
        assert_eq!(issue.kind, IssueKind::Missing);
        let remove = format!("ctx workspace remove {} --yes", workspace.id);
        assert!(
            issue.recovery.iter().any(|step| step.contains(&remove)),
            "{issue:?}"
        );
        assert!(state.unavailable_workspaces.contains(&workspace.id));
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("[MISSING]")),
            "{:?}",
            state.message
        );
        assert_eq!(
            active_workspace_id(&app),
            Some(workspace.id),
            "loading never changes the active workspace"
        );
        let mut home = state.clone();
        home.screen = Screen::Home;
        let rendered = draw(&home, 120, 36);
        assert!(rendered.contains("[MISSING]"), "{rendered}");
        assert!(
            rendered.contains("Workspace directory not found"),
            "{rendered}"
        );
        let mut listing = state.clone();
        listing.screen = Screen::Workspaces;
        assert!(draw(&listing, 120, 36).contains("[MISSING]"));
    }

    // tui#1: Workspaces Enter inspects before it persists the active workspace.
    #[test]
    fn opening_a_workspace_validates_it_before_it_becomes_active() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let stale_directory = workspace_dir(root.path(), "stale");
        let stale = app
            .register_workspace(&stale_directory, TrustState::Untrusted, false)
            .expect("stale");
        let other = app
            .register_workspace(
                &workspace_dir(root.path(), "other"),
                TrustState::Untrusted,
                false,
            )
            .expect("other");
        let good = app
            .register_workspace(
                &workspace_dir(root.path(), "good"),
                TrustState::Untrusted,
                true,
            )
            .expect("good");
        std::fs::remove_dir_all(&stale_directory).expect("delete");
        let cache = ProviderCache {
            agents: Vec::new(),
            probed_with: None,
        };

        let refused = execute_action(
            &app,
            Action::OpenWorkspace {
                workspace: stale.clone(),
            },
            &cache,
        )
        .expect_err("a missing directory is never opened");
        assert!(
            refused.contains("The active workspace was not changed"),
            "{refused}"
        );
        assert!(
            refused.contains(&format!("ctx workspace remove {} --yes", stale.id)),
            "{refused}"
        );
        assert_eq!(active_workspace_id(&app), Some(good.id));

        let opened = execute_action(
            &app,
            Action::OpenWorkspace {
                workspace: other.clone(),
            },
            &cache,
        )
        .expect("an existing directory opens");
        assert_eq!(active_workspace_id(&app), Some(other.id));
        assert_eq!(opened.local.status.workspace.id, other.id);
        assert!(opened.local.unavailable_workspaces.contains(&stale.id));
        assert!(
            opened.message.contains("Opened other"),
            "{}",
            opened.message
        );
    }

    // tui#1: one unreadable checkpoint is reported with its recovery command.
    #[test]
    fn unreadable_checkpoint_is_reported_without_hiding_the_others() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let workspace = app
            .register_workspace(
                &workspace_dir(root.path(), "workspace"),
                TrustState::Untrusted,
                true,
            )
            .expect("register");
        let create = |objective: &str| {
            app.checkpoints()
                .create(
                    &workspace,
                    None,
                    None,
                    CheckpointInput {
                        objective: objective.into(),
                        ..CheckpointInput::default()
                    },
                )
                .expect("checkpoint")
        };
        let kept = create("Keep this checkpoint");
        let damaged = create("Damaged checkpoint");
        std::fs::remove_file(
            app.paths
                .checkpoints_dir()
                .join(format!("{}.json", damaged.id)),
        )
        .expect("delete checkpoint file");

        let mut state = UiState::load(&app, false, &NoAgents);

        assert!(state.checkpoints.iter().any(|item| item.id == kept.id));
        assert!(state.checkpoints.iter().all(|item| item.id != damaged.id));
        let hint = format!("ctx checkpoint delete {} --yes", damaged.id);
        assert!(
            state.issues.iter().any(|issue| issue.contains(&hint)),
            "{:?}",
            state.issues
        );
        state.screen = Screen::Diagnostics;
        let diagnostics = draw(&state, 160, 40);
        assert!(diagnostics.contains("Local state"), "{diagnostics}");
    }

    // tui#2: invalid input keeps the prompt open with a reason instead of exiting.
    #[test]
    fn profile_prompt_keeps_rejected_names_open_with_an_explanation() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        state.screen = Screen::Profiles;
        for (name, expected) in [
            ("work", "already exists"),
            ("WORK", "already exists"),
            ("team/a", "path separators"),
            ("!!!", "letter or number"),
            ("   ", "cannot be empty"),
        ] {
            state.input = InputMode::AddProfile {
                value: name.into(),
                agent_index: 0,
            };
            assert!(send(&mut state, &app, KeyCode::Enter).is_none(), "{name}");
            assert!(
                matches!(&state.input, InputMode::AddProfile { value, .. } if value == name),
                "{name}: the prompt stays open with its value"
            );
            assert!(
                state
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains(expected)),
                "{name}: {:?}",
                state.message
            );
        }
        state.input = InputMode::AddProfile {
            value: "Personal".into(),
            agent_index: 0,
        };
        let action = send(&mut state, &app, KeyCode::Enter);
        assert!(
            matches!(&action, Some(Action::CreateProfile { name, agent_id, .. }) if name == "Personal" && agent_id == "codex"),
            "{action:?}"
        );
    }

    // tui#2: a failed action is a message; the TUI and the prompt stay open.
    #[test]
    fn failed_action_keeps_the_tui_running_and_the_prompt_open() {
        let mut state = fixture();
        state.screen = Screen::Checkpoints;
        state.input = InputMode::CreateCheckpoint {
            value: "Ship it".into(),
        };
        let now = Instant::now();
        let action = Action::CreateCheckpoint {
            workspace: state.status.workspace.clone(),
            objective: "Ship it".into(),
        };
        state.begin_action(&action, now);
        state.finish_action(
            Err("Could not create a checkpoint for Example Workspace: Git command failed.".into()),
            now,
        );
        assert!(state.busy.is_none());
        assert!(
            matches!(&state.input, InputMode::CreateCheckpoint { value } if value == "Ship it")
        );
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Could not create a checkpoint"))
        );
        assert!(state.exit.is_none());
        assert!(state.refresh_requested, "local state is re-read afterwards");
    }

    // tui#2: the TUI applies resume_session's guards before it tears down.
    #[test]
    fn native_resume_requires_the_sessions_agent_and_profile() {
        let mut state = fixture();
        let workspace_id = state.status.workspace.id;
        let active = state.status.active_profile.clone().expect("active profile");
        state.screen = Screen::SessionDetail;
        let mut session = test_session(workspace_id, "Claude work", "claude");
        state.sessions = vec![session.clone()];
        assert!(!state.queue_selected_session());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("belongs to agent claude"))
        );
        session.agent_id = "codex".into();
        session.profile_id = Some(Uuid::new_v4());
        state.sessions = vec![session.clone()];
        assert!(!state.queue_selected_session());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("another profile"))
        );
        assert!(state.launch_action.is_none());
        session.profile_id = Some(active.id);
        state.sessions = vec![session];
        assert!(state.queue_selected_session());
        assert!(state.launch_action.is_some());
    }

    // tui#2 end to end: an action that fails on the worker never ends the loop.
    #[test]
    fn failing_actions_never_end_the_tui() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let stale_directory = workspace_dir(root.path(), "stale");
        app.register_workspace(&stale_directory, TrustState::Untrusted, false)
            .expect("stale");
        let good = app
            .register_workspace(
                &workspace_dir(root.path(), "good"),
                TrustState::Untrusted,
                true,
            )
            .expect("good");
        std::fs::remove_dir_all(&stale_directory).expect("delete");
        let probe: Arc<dyn AgentProbe> = Arc::new(NoAgents);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        let loaded = UiState::load(&app, false, probe.as_ref());
        // W opens Workspaces, where the stale entry is second (least recently used);
        // Enter tries to open it; Ctrl+C quits once that attempt has finished.
        let mut events = ScriptedEvents::new(vec![
            vec![press(KeyCode::Char('w'))],
            vec![press(KeyCode::Down)],
            vec![press(KeyCode::Enter)],
            vec![Event::Key(ctrl_c())],
        ]);

        let state = event_loop(&mut terminal, &app, false, &mut events, &probe, loaded)
            .expect("the TUI keeps running");

        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("The active workspace was not changed")),
            "{:?}",
            state.message
        );
        assert_eq!(active_workspace_id(&app), Some(good.id));
        assert!(state.launch_action.is_none());
    }

    // tui#3: the switch runs off the UI thread; buffered keys cannot launch.
    #[test]
    fn identity_switch_runs_in_the_background_and_buffered_keys_cannot_launch() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let workspace = app
            .register_workspace(
                &workspace_dir(root.path(), "workspace"),
                TrustState::Untrusted,
                true,
            )
            .expect("register");
        let work = app.create_profile("Work", None, None).expect("work");
        let personal = app
            .create_profile("Personal", None, None)
            .expect("personal");
        // A checkpoint with a profile: ContextWake's own validator still rejects
        // handoffs from profile-less checkpoints (audit app#2, outside this package).
        let checkpoint = app
            .checkpoints()
            .create(
                &workspace,
                Some(&work),
                None,
                CheckpointInput {
                    objective: "Continue after the switch".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        let handoff = app
            .handoffs()
            .create(&checkpoint, &workspace)
            .expect("handoff");
        let mut state = UiState::load(&app, false, &NoAgents);
        state.screen = Screen::Profiles;
        state.selected = state
            .profiles
            .iter()
            .position(|profile| profile.id == personal.id)
            .expect("personal is listed");

        assert!(send(&mut state, &app, KeyCode::Enter).is_none());
        assert!(
            matches!(state.input, InputMode::ConfirmSwitch { target } if target == personal.id)
        );
        let action = send(&mut state, &app, KeyCode::Enter).expect("Enter confirms");
        assert!(matches!(
            &action,
            Action::SwitchProfile { target, workspace: Some(_) } if target.id == personal.id
        ));
        let started = Instant::now();
        state.begin_action(&action, started);

        // While the worker runs, keys are dropped instead of queued for later.
        for code in [KeyCode::Enter, KeyCode::Char('x'), KeyCode::Char('q')] {
            assert!(
                state
                    .dispatch(&app, Input::Key(key(code)), started)
                    .is_none()
            );
        }
        assert!(state.exit.is_none());
        assert!(state.launch_action.is_none());
        let busy = draw(&state, 100, 30);
        assert!(busy.contains("WORKING"), "{busy}");

        let local = LocalSnapshot::collect(&app, unconfigured_agent_health());
        state.finish_action(
            Ok(ActionOutput {
                message: "Profile switched.".into(),
                local: Box::new(local),
                next: NextView::Preview(handoff.id),
            }),
            started,
        );
        assert_eq!(state.screen, Screen::HandoffPreview);
        assert_eq!(state.handoffs[state.selected].id, handoff.id);

        // An Enter right after the preview opened by itself is not a reviewed decision.
        let early = started + Duration::from_millis(100);
        assert!(
            state
                .dispatch(&app, Input::Key(key(KeyCode::Enter)), early)
                .is_none()
        );
        assert!(state.launch_action.is_none());
        assert!(state.exit.is_none());

        // A deliberate Enter after the review delay starts the new session.
        let reviewed = started + AUTO_OPEN_ARMING;
        assert!(
            state
                .dispatch(&app, Input::Key(key(KeyCode::Enter)), reviewed)
                .is_none()
        );
        assert_eq!(
            state.launch_action,
            Some(LaunchAction::ContinueHandoff(handoff.id.to_string())),
            "{:?}",
            state.message
        );
        assert!(state.exit.is_some());
    }

    // tui#4: lists scroll so the selection stays visible.
    #[test]
    fn long_lists_scroll_to_keep_the_selection_visible() {
        let mut state = fixture();
        let workspace_id = state.status.workspace.id;
        state.all_sessions = (0..40)
            .map(|index| test_session(workspace_id, &format!("Session {index:02}"), "codex"))
            .collect();
        state.apply_session_filter();
        state.checkpoints = (0..40)
            .map(|index| test_checkpoint(workspace_id, &format!("Objective {index:02}")))
            .collect();
        let checkpoint_id = state.checkpoints[0].id;
        state.handoffs = (0..40)
            .map(|index| test_handoff(checkpoint_id, &format!("v{index:02}")))
            .collect();
        state.workspaces.extend((0..40).map(|index| {
            test_workspace(
                PathBuf::from(format!("C:/work/repo-{index:02}")),
                &format!("Workspace {index:02}"),
            )
        }));
        state.profiles.extend((0..40).map(|index| {
            test_profile(
                &format!("p{index:02}"),
                &format!("Profile {index:02}"),
                format!("C:/state/codex/{index:02}"),
                AuthState::SignedOut,
            )
        }));
        for (screen, expected) in [
            (Screen::Sessions, "> Session 37"),
            (Screen::Checkpoints, "Objective 37"),
            (Screen::Handoffs, "schema v37"),
            (Screen::Workspaces, "Workspace 36"),
            (Screen::Profiles, "Profile 36"),
        ] {
            state.screen = screen;
            state.selected = 37;
            state.list_offset.set(0);
            let rendered = draw(&state, 80, 24);
            assert!(rendered.contains(expected), "{screen:?}: {rendered}");
        }
    }

    // tui#5: pasted text never acts as hotkeys and never submits a prompt.
    #[test]
    fn pasted_keys_never_run_hotkeys_or_submit_prompts() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        state.screen = Screen::Checkpoints;
        // A Windows console delivers a paste as queued key presses (each followed by
        // its release), with every line break as Enter.
        let burst = "od\r\rq"
            .chars()
            .flat_map(|character| {
                let code = if character == '\r' {
                    KeyCode::Enter
                } else {
                    KeyCode::Char(character)
                };
                [press(code), release(code)]
            })
            .collect::<Vec<_>>();
        let (inputs, pasted) = classify_batch(burst, false);
        assert!(pasted);
        assert_eq!(inputs.len(), 1);
        for input in inputs {
            assert!(state.dispatch(&app, input, Instant::now()).is_none());
        }
        assert_eq!(state.screen, Screen::Checkpoints);
        assert!(state.exit.is_none());
        assert!(state.launch_action.is_none());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Pasted text was ignored"))
        );

        // In a prompt, pasted text becomes one line and nothing is submitted.
        state.input = InputMode::CreateCheckpoint {
            value: "Fix ".into(),
        };
        let (inputs, _) = classify_batch(
            vec![Event::Paste(
                "Refactor auth\r\nTODO \u{1b}[31mred\u{1b}[0m\n\n".into(),
            )],
            false,
        );
        for input in inputs {
            assert!(state.dispatch(&app, input, Instant::now()).is_none());
        }
        assert!(
            matches!(&state.input, InputMode::CreateCheckpoint { value } if value.trim_end() == "Fix Refactor auth TODO red"),
            "{:?}",
            state.input
        );

        // The identity confirmation cannot be answered by a paste.
        state.input = InputMode::ConfirmSwitch {
            target: state.profiles[0].id,
        };
        let (inputs, _) = classify_batch(
            vec![press(KeyCode::Char('x')), press(KeyCode::Enter)],
            false,
        );
        for input in inputs {
            assert!(state.dispatch(&app, input, Instant::now()).is_none());
        }
        assert!(matches!(state.input, InputMode::ConfirmSwitch { .. }));
    }

    // Windows reports releases as separate events; nothing may act twice.
    #[test]
    fn key_releases_and_held_navigation_keys_act_once_per_press() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let (inputs, pasted) =
            classify_batch(vec![press(KeyCode::Down), release(KeyCode::Down)], false);
        assert!(!pasted);
        assert_eq!(inputs, vec![Input::Key(key(KeyCode::Down))]);
        assert!(
            classify_batch(vec![release(KeyCode::Enter)], false)
                .0
                .is_empty()
        );
        let (inputs, pasted) = classify_batch(
            vec![
                press(KeyCode::Down),
                press(KeyCode::Down),
                press(KeyCode::Down),
            ],
            false,
        );
        assert!(!pasted, "held arrow keys still scroll");
        assert_eq!(inputs.len(), 3);

        let mut state = fixture();
        state.screen = Screen::Profiles;
        let enter_release =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(
            state
                .dispatch(&app, Input::Key(enter_release), Instant::now())
                .is_none()
        );
        assert!(matches!(state.input, InputMode::Normal));
    }

    // tui#7: exit waits are bounded and Ctrl+C is a real interrupt.
    #[test]
    fn exit_waits_for_provider_probes_are_bounded() {
        let start = Instant::now();
        let exit = ExitRequest {
            requested_at: start,
            interrupts: 0,
        };
        assert!(!exit_ready(exit, start, true, false));
        assert!(!exit_ready(
            exit,
            start + Duration::from_secs(1),
            true,
            false
        ));
        assert!(exit_ready(exit, start + EXIT_WAIT_LIMIT, true, false));
        assert!(exit_ready(exit, start, false, false));
        let interrupted = ExitRequest {
            interrupts: 1,
            ..exit
        };
        assert!(exit_ready(interrupted, start, true, false));
        assert!(
            !exit_ready(interrupted, start, true, true),
            "a running local mutation may finish"
        );
        let forced = ExitRequest {
            interrupts: 2,
            ..exit
        };
        assert!(exit_ready(forced, start, true, true));
    }

    #[test]
    fn ctrl_c_quits_and_esc_cancels_a_waiting_launch() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let now = Instant::now();
        let mut state = fixture();
        assert!(state.dispatch(&app, Input::Key(ctrl_c()), now).is_none());
        assert_eq!(
            state.screen,
            Screen::Home,
            "Ctrl+C no longer opens Checkpoints"
        );
        assert_eq!(state.exit.map(|exit| exit.interrupts), Some(1));

        let mut waiting = fixture();
        waiting.refreshing = true;
        waiting.launch_action = Some(LaunchAction::ResumeSession("session".into()));
        waiting.request_exit(now);
        assert!(
            waiting
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Esc cancels"))
        );
        assert!(send(&mut waiting, &app, KeyCode::Esc).is_none());
        assert!(waiting.exit.is_none());
        assert!(waiting.launch_action.is_none(), "Esc cancels the launch");

        waiting.launch_action = Some(LaunchAction::ResumeSession("session".into()));
        waiting.request_exit(now);
        assert!(waiting.dispatch(&app, Input::Key(ctrl_c()), now).is_none());
        assert!(waiting.launch_action.is_none(), "Ctrl+C never launches");
        assert!(waiting.exit.is_some());
    }

    #[test]
    fn loading_screen_quits_while_a_probe_is_stuck() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        app.register_workspace(
            &workspace_dir(root.path(), "workspace"),
            TrustState::Untrusted,
            true,
        )
        .expect("register");
        let (release_probe, stuck) = mpsc::channel::<()>();
        let probe: Arc<dyn AgentProbe> = Arc::new(StuckProbe {
            release: Mutex::new(stuck),
            _root: root,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        let started = Instant::now();

        let mut interrupt = ScriptedEvents::new(vec![vec![Event::Key(ctrl_c())]]);
        let loaded = load_state_responsively(&mut terminal, &app, false, &mut interrupt, &probe)
            .expect("loading");
        assert!(loaded.is_none());

        let mut quit_twice = ScriptedEvents::new(vec![
            vec![press(KeyCode::Char('q'))],
            vec![press(KeyCode::Char('q'))],
        ]);
        let loaded = load_state_responsively(&mut terminal, &app, false, &mut quit_twice, &probe)
            .expect("loading");
        assert!(loaded.is_none());
        assert!(
            started.elapsed() < EXIT_WAIT_LIMIT,
            "quitting never waited for the stuck probe"
        );
        drop(release_probe);
    }

    // tui#8: Windows AltGr (Ctrl+Alt) characters are text.
    #[test]
    fn altgr_characters_are_typed_into_every_prompt() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        for mode in [
            InputMode::AddProfile {
                value: String::new(),
                agent_index: 0,
            },
            InputMode::CreateCheckpoint {
                value: String::new(),
            },
            InputMode::SearchSessions {
                value: String::new(),
            },
        ] {
            state.input = mode;
            for character in ['@', '\\', '{', 'ł'] {
                let typed = KeyEvent::new(KeyCode::Char(character), altgr);
                assert!(
                    state
                        .dispatch(&app, Input::Key(typed), Instant::now())
                        .is_none()
                );
            }
            // Ctrl without Alt is a shortcut, never text.
            let shortcut = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
            assert!(
                state
                    .dispatch(&app, Input::Key(shortcut), Instant::now())
                    .is_none()
            );
            let typed = state.input.text_mut().map(|(value, _)| value.clone());
            assert_eq!(typed.as_deref(), Some("@\\{ł"), "{:?}", state.input);
        }
    }

    // tui#9: a modal hidden by a tiny terminal cannot be answered blindly.
    #[test]
    fn tiny_terminal_pauses_a_hidden_confirmation() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        let personal = test_profile(
            "personal",
            "Personal",
            "C:/state/codex/personal",
            AuthState::SignedOut,
        );
        state.profiles.push(personal.clone());
        state.input = InputMode::ConfirmSwitch {
            target: personal.id,
        };
        let tiny = draw(&state, 40, 10);
        assert!(tiny.contains("CONFIRM IDENTITY CHANGE"), "{tiny}");
        for code in [KeyCode::Enter, KeyCode::Char('x'), KeyCode::Char('X')] {
            assert!(send(&mut state, &app, code).is_none(), "{code:?}");
            assert!(matches!(state.input, InputMode::ConfirmSwitch { .. }));
        }
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("too small"))
        );
        assert!(send(&mut state, &app, KeyCode::Esc).is_none());
        assert!(matches!(state.input, InputMode::Normal));

        // Once the terminal is large enough again, the same keys work.
        draw(&state, 80, 24);
        state.input = InputMode::ConfirmSwitch {
            target: personal.id,
        };
        assert!(matches!(
            send(&mut state, &app, KeyCode::Char('x')),
            Some(Action::SwitchProfile { .. })
        ));
    }

    // tui#9: the answer keys stay visible just above the minimum size.
    #[test]
    fn confirmation_answers_stay_visible_at_the_minimum_size() {
        let mut state = fixture();
        let personal = test_profile(
            "personal",
            "Personal",
            "C:/state/codex/personal",
            AuthState::SignedOut,
        );
        state.profiles.push(personal.clone());
        state.input = InputMode::ConfirmSwitch {
            target: personal.id,
        };
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (50, 14), (120, 36)] {
            let rendered = draw(&state, width, height);
            for answer in ["[Enter]", "[X]", "[Esc]"] {
                assert!(
                    rendered.contains(answer),
                    "{width}x{height} {answer}: {rendered}"
                );
            }
        }
    }

    // tui#10: provider results probed with another profile's home are not applied.
    #[test]
    fn cached_provider_state_is_never_applied_to_another_profile() {
        let state = fixture();
        let work = state.status.active_profile.clone().expect("work");
        let personal = test_profile(
            "personal",
            "Personal",
            "C:/state/codex/personal",
            AuthState::SignedOut,
        );
        let other_home =
            cached_status_health(Some(&personal), &state.agents, state.probed_with.as_ref());
        assert!(!other_home.installed, "never persisted for Personal");
        assert_eq!(other_home.auth_state, AuthState::SignedOut);
        assert_eq!(other_home.version.as_deref(), Some("codex-cli 0.154.0"));
        let same_home =
            cached_status_health(Some(&work), &state.agents, state.probed_with.as_ref());
        assert!(same_home.installed);
        assert_eq!(same_home.auth_state, AuthState::SignedIn);
        assert!(!cached_status_health(Some(&work), &state.agents, None).installed);
    }

    #[test]
    fn switching_profiles_keeps_the_new_profiles_stored_sign_in_state() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        app.register_workspace(
            &workspace_dir(root.path(), "workspace"),
            TrustState::Untrusted,
            true,
        )
        .expect("register");
        let work = test_profile(
            "work",
            "Work",
            root.path().join("homes").join("work"),
            AuthState::SignedIn,
        );
        let personal = test_profile(
            "personal",
            "Personal",
            root.path().join("homes").join("personal"),
            AuthState::SignedOut,
        );
        app.store.insert_profile(&work).expect("work");
        app.store.insert_profile(&personal).expect("personal");
        app.store
            .set_active_profile(work.id)
            .expect("activate work");
        let mut state = UiState::load(&app, false, &NoAgents);
        // Cached provider results probed with Work's home: installed and signed in.
        let codex = state
            .agents
            .iter_mut()
            .find(|agent| agent.id == "codex")
            .expect("codex adapter");
        codex.detected_version = Some("codex-cli 1.0.0".into());
        codex.auth_state = AuthState::SignedIn;

        app.store
            .set_active_profile(personal.id)
            .expect("switch to personal");
        let local = LocalSnapshot::collect(&app, state.provider_cache().status_health(&app));

        assert_eq!(
            app.store.profile("personal").expect("personal").auth_state,
            AuthState::SignedOut,
            "Work's sign-in state must not be written to Personal"
        );
        assert_eq!(
            local
                .status
                .active_profile
                .map(|profile| profile.auth_state),
            Some(AuthState::SignedOut)
        );
    }

    // tui#11: background refreshes keep the search filter and record identity.
    #[test]
    fn background_refresh_keeps_the_session_search_and_selection() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        let workspace_id = state.status.workspace.id;
        let docs = test_session(workspace_id, "Update docs", "codex");
        let auth = test_session(workspace_id, "Auth refactor", "codex");
        state.all_sessions = vec![docs.clone(), auth.clone()];
        state.apply_session_filter();
        state.screen = Screen::Sessions;
        state.input = InputMode::SearchSessions {
            value: "auth".into(),
        };
        assert!(send(&mut state, &app, KeyCode::Enter).is_none());
        let ids = |state: &UiState| {
            state
                .sessions
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&state), vec![auth.id]);
        let message = state.message.clone();

        let mut refreshed = fixture();
        refreshed.all_sessions = vec![
            test_session(workspace_id, "Newest", "codex"),
            docs,
            auth.clone(),
        ];
        refreshed.apply_session_filter();
        state.apply_background_refresh(refreshed);

        assert_eq!(
            ids(&state),
            vec![auth.id],
            "the filter survives the refresh"
        );
        assert_eq!(state.sessions[state.selected].id, auth.id);
        assert_eq!(state.message, message);
        assert!(send(&mut state, &app, KeyCode::Char('r')).is_none());
        assert_eq!(state.sessions.len(), 3, "R clears the search");
    }

    #[test]
    fn refreshes_keep_confirmation_and_detail_targets_by_identity() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let mut state = fixture();
        let work = state.profiles[0].clone();
        let personal = test_profile(
            "personal",
            "Personal",
            "C:/state/codex/personal",
            AuthState::SignedOut,
        );
        let other = test_profile(
            "other",
            "Other",
            "C:/state/codex/other",
            AuthState::SignedOut,
        );
        state.profiles = vec![work.clone(), personal.clone(), other.clone()];
        state.screen = Screen::Profiles;
        state.selected = 1;
        assert!(send(&mut state, &app, KeyCode::Enter).is_none());
        let mut refreshed = fixture();
        refreshed.profiles = vec![other, work, personal.clone()];
        state.apply_background_refresh(refreshed);
        let action = send(&mut state, &app, KeyCode::Char('x')).expect("X switches");
        assert!(
            matches!(&action, Action::SwitchProfile { target, .. } if target.id == personal.id),
            "{action:?}"
        );

        // A detail screen whose record disappeared returns to its list instead of
        // showing (and resuming) a different record.
        let mut state = fixture();
        let workspace_id = state.status.workspace.id;
        let first = test_session(workspace_id, "First", "codex");
        state.all_sessions = vec![first.clone(), test_session(workspace_id, "Gone", "codex")];
        state.apply_session_filter();
        state.screen = Screen::SessionDetail;
        state.selected = 1;
        let mut refreshed = fixture();
        refreshed.all_sessions = vec![first];
        refreshed.apply_session_filter();
        state.apply_background_refresh(refreshed);
        assert_eq!(state.screen, Screen::Sessions);
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("no longer available"))
        );
    }

    // tui#2: a handoff that fails validation is refused while the TUI is still
    // open, instead of failing at the shell after the terminal was restored.
    #[test]
    fn a_damaged_handoff_is_refused_before_the_tui_exits() {
        let root = tempfile::tempdir().expect("root");
        let app = test_app(root.path());
        let workspace = app
            .register_workspace(
                &workspace_dir(root.path(), "workspace"),
                TrustState::Untrusted,
                true,
            )
            .expect("register");
        let work = app.create_profile("Work", None, None).expect("work");
        let checkpoint = app
            .checkpoints()
            .create(
                &workspace,
                Some(&work),
                None,
                CheckpointInput {
                    objective: "Continue the work".into(),
                    ..CheckpointInput::default()
                },
            )
            .expect("checkpoint");
        let handoff = app
            .handoffs()
            .create(&checkpoint, &workspace)
            .expect("handoff");
        std::fs::write(handoff.storage_path.join("context.md"), "tampered")
            .expect("tamper with the package");
        let mut state = UiState::load(&app, false, &NoAgents);
        state.screen = Screen::HandoffPreview;
        state.selected = state
            .handoffs
            .iter()
            .position(|item| item.id == handoff.id)
            .expect("listed");

        assert!(send(&mut state, &app, KeyCode::Enter).is_none());

        assert!(state.launch_action.is_none());
        assert!(state.exit.is_none());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("cannot be started")),
            "{:?}",
            state.message
        );
    }

    #[test]
    fn input_queued_during_an_action_is_dropped_but_ctrl_c_still_quits() {
        let mut typed_ahead = ScriptedEvents::new(vec![vec![
            press(KeyCode::Enter),
            release(KeyCode::Enter),
            press(KeyCode::Enter),
        ]]);
        assert!(typed_ahead.poll(Duration::from_millis(1)).expect("poll"));
        assert!(!discard_queued_input(&mut typed_ahead).expect("discard"));
        assert!(!typed_ahead.poll(Duration::ZERO).expect("poll"));

        let mut interrupted =
            ScriptedEvents::new(vec![vec![press(KeyCode::Enter), Event::Key(ctrl_c())]]);
        assert!(interrupted.poll(Duration::from_millis(1)).expect("poll"));
        assert!(discard_queued_input(&mut interrupted).expect("discard"));
    }

    #[test]
    fn a_queued_launch_whose_record_disappears_is_cancelled() {
        let mut state = fixture();
        let handoff = test_handoff(state.checkpoints[0].id, "1.2.0");
        state.handoffs = vec![handoff];
        state.screen = Screen::HandoffPreview;
        state.refreshing = true;
        assert!(state.queue_selected_handoff());
        state.request_exit(Instant::now());

        // The bounded exit wait ends with a refresh in which the handoff is gone.
        let refreshed = fixture();
        state.apply_background_refresh(refreshed);

        assert_eq!(state.screen, Screen::Handoffs);
        assert!(state.launch_action.is_none());
        assert!(state.exit.is_none());
        assert!(
            state
                .message
                .as_deref()
                .is_some_and(|message| message.contains("nothing was launched"))
        );
    }

    // tui#6: launch review names the target workspace; another workspace needs a
    // second Enter.
    #[test]
    fn launch_review_names_the_target_workspace_and_confirms_other_workspaces() {
        let mut state = fixture();
        let backend = test_workspace(PathBuf::from("C:/work/backend"), "backend");
        let checkpoint = test_checkpoint(backend.id, "Backend objective");
        let handoff = test_handoff(checkpoint.id, "1.2.0");
        state.workspaces.push(backend.clone());
        state.checkpoints.push(checkpoint);
        state.handoffs = vec![handoff.clone()];
        state.screen = Screen::Handoffs;
        let list = draw(&state, 120, 36);
        assert!(list.contains("backend [OTHER WORKSPACE]"), "{list}");

        state.screen = Screen::HandoffPreview;
        let preview = draw(&state, 120, 40);
        assert!(preview.contains("C:/work/backend"), "{preview}");
        assert!(preview.contains("[OTHER WORKSPACE]"), "{preview}");
        assert!(
            !state.queue_selected_handoff(),
            "the first Enter only warns"
        );
        assert!(state.launch_action.is_none());
        assert!(
            state.message.as_deref().is_some_and(
                |message| message.contains("backend") && message.contains("Enter again")
            )
        );
        assert!(state.queue_selected_handoff());
        assert_eq!(
            state.launch_action,
            Some(LaunchAction::ContinueHandoff(handoff.id.to_string()))
        );

        state.all_sessions = vec![test_session(backend.id, "Backend session", "codex")];
        state.apply_session_filter();
        state.screen = Screen::SessionDetail;
        state.selected = 0;
        let detail = draw(&state, 120, 36);
        assert!(detail.contains("backend [OTHER WORKSPACE]"), "{detail}");
        assert!(detail.contains("C:/work/backend"), "{detail}");
    }

    // tui#12: the terminal is restored on setup failure, teardown failure and a
    // UI-thread panic, and left alone for worker panics.
    #[derive(Default)]
    struct ModeLog {
        calls: Mutex<Vec<&'static str>>,
        failing: Vec<&'static str>,
    }

    struct RecordingModes(Arc<ModeLog>);

    impl RecordingModes {
        fn step(&self, name: &'static str) -> io::Result<()> {
            self.0.calls.lock().expect("log").push(name);
            if self.0.failing.contains(&name) {
                Err(io::Error::other(format!("{name} failed")))
            } else {
                Ok(())
            }
        }
    }

    impl TerminalModes for RecordingModes {
        fn enable_raw_mode(&self) -> io::Result<()> {
            self.step("enable_raw_mode")
        }

        fn disable_raw_mode(&self) -> io::Result<()> {
            self.step("disable_raw_mode")
        }

        fn enter_alternate_screen(&self) -> io::Result<()> {
            self.step("enter_alternate_screen")
        }

        fn leave_alternate_screen(&self) -> io::Result<()> {
            self.step("leave_alternate_screen")
        }

        fn enable_bracketed_paste(&self) -> io::Result<()> {
            self.step("enable_bracketed_paste")
        }

        fn disable_bracketed_paste(&self) -> io::Result<()> {
            self.step("disable_bracketed_paste")
        }

        fn show_cursor(&self) -> io::Result<()> {
            self.step("show_cursor")
        }
    }

    fn mode_log(failing: &[&'static str]) -> Arc<ModeLog> {
        Arc::new(ModeLog {
            calls: Mutex::new(Vec::new()),
            failing: failing.to_vec(),
        })
    }

    fn recorded(log: &ModeLog) -> Vec<&'static str> {
        log.calls.lock().expect("log").clone()
    }

    fn count(log: &ModeLog, step: &str) -> usize {
        recorded(log).iter().filter(|call| **call == step).count()
    }

    #[test]
    fn terminal_is_restored_on_setup_failure_teardown_failure_and_panic() {
        // Setup fails after raw mode was enabled.
        let log = mode_log(&["enter_alternate_screen"]);
        assert!(TerminalSession::start(Box::new(RecordingModes(Arc::clone(&log)))).is_err());
        assert_eq!(count(&log, "disable_raw_mode"), 1, "{:?}", recorded(&log));
        assert_eq!(count(&log, "show_cursor"), 1, "{:?}", recorded(&log));

        // A failing restore step does not skip the others, and nothing runs twice.
        let log = mode_log(&["disable_raw_mode"]);
        let mut session =
            TerminalSession::start(Box::new(RecordingModes(Arc::clone(&log)))).expect("start");
        assert!(session.restore().is_err());
        for step in [
            "disable_bracketed_paste",
            "leave_alternate_screen",
            "disable_raw_mode",
            "show_cursor",
        ] {
            assert_eq!(count(&log, step), 1, "{step}: {:?}", recorded(&log));
        }
        drop(session);
        assert_eq!(count(&log, "disable_raw_mode"), 1);

        // A worker panic leaves the TUI's terminal alone; a UI-thread panic restores
        // it before the panic is reported.
        let log = mode_log(&[]);
        let mut session =
            TerminalSession::start(Box::new(RecordingModes(Arc::clone(&log)))).expect("start");
        let worker: thread::JoinHandle<()> =
            thread::spawn(|| panic!("worker panic expected by this test"));
        assert!(worker.join().is_err());
        assert_eq!(count(&log, "disable_raw_mode"), 0);
        let caught: thread::Result<()> =
            std::panic::catch_unwind(|| panic!("UI panic expected by this test"));
        assert!(caught.is_err());
        assert_eq!(
            count(&log, "leave_alternate_screen"),
            1,
            "{:?}",
            recorded(&log)
        );
        assert_eq!(count(&log, "disable_raw_mode"), 1, "{:?}", recorded(&log));
        assert!(session.restore().is_ok());
        assert_eq!(count(&log, "disable_raw_mode"), 1, "restored only once");
    }

    #[test]
    fn background_tasks_report_results_and_panics_and_never_block_on_drop() {
        fn wait<T: Send + 'static>(task: &mut Task<T>) -> TaskPoll<T> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match task.poll() {
                    TaskPoll::Pending => assert!(Instant::now() < deadline, "task timed out"),
                    finished => return finished,
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        let mut answer = Task::spawn("test-answer", || 41 + 1).expect("spawn");
        assert!(matches!(wait(&mut answer), TaskPoll::Ready(42)));
        let mut failing: Task<u8> =
            Task::spawn("test-panic", || panic!("task panic expected by this test"))
                .expect("spawn");
        assert!(matches!(wait(&mut failing), TaskPoll::Failed));
        let (release_task, blocked) = mpsc::channel::<()>();
        let pending = Task::spawn("test-blocked", move || blocked.recv().is_ok()).expect("spawn");
        let dropped_at = Instant::now();
        drop(pending);
        assert!(dropped_at.elapsed() < Duration::from_secs(1));
        drop(release_task);
    }

    // A timed-out git status is not reported as a clean workspace.
    #[test]
    fn timed_out_git_status_is_never_shown_as_clean() {
        let mut state = fixture();
        state.status.git = Some(GitSnapshot {
            repository_root: state.status.workspace.git_root.clone(),
            timed_out: true,
            ..GitSnapshot::default()
        });
        let home = draw(&state, 120, 36);
        assert!(home.contains("[GIT TIMEOUT]"), "{home}");
        assert!(home.contains("timed out"), "{home}");
        assert!(!home.contains("[CLEAN]"), "{home}");
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_workspace_paths_are_shown_in_conventional_form() {
        let mut state = fixture();
        state.workspaces[0].path = PathBuf::from(r"\\?\C:\Work\Repo");
        state.screen = Screen::Workspaces;
        let rendered = draw(&state, 100, 30);
        assert!(rendered.contains(r"C:\Work\Repo"), "{rendered}");
        assert!(!rendered.contains(r"\\?\"), "{rendered}");
    }
}
