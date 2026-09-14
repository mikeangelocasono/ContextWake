/*
THESIS: ContextWake is a continuity console: identity, workspace evidence, and the safe next action stay visible together.
OWN-WORLD: graphite terminal surfaces, cyan focus, amber warnings, square single-line frames, and explicit text markers.
STORY: inspect real local/agent state, choose a profile deliberately, preserve project context, then continue with the selected coding agent.
FIRST VIEWPORT: compact title/status rail, profile lane, workspace/Git evidence, continuity panel, and keyboard action bar.
FORM: dense operator console benchmarked against Lazygit and GitHub CLI, with ContextWake's continuity decision as the center.
FINISH: unreviewed and undocumented is unfinished; this build ends with a reviewed, documented terminal workflow.
*/

use std::io;
use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
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
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};

use crate::app::{Application, StatusView};
use crate::checkpoint::CheckpointInput;
use crate::doctor::{DoctorReport, run_doctor_with_agent_results};
use crate::error::{ContextWakeError, Result};
use crate::model::{
    AuthState, Capability, Checkpoint, CodingAgent, HandoffRecord, Profile, Session, UsageSummary,
    Workspace,
};
use crate::{PRODUCT_NAME, VERSION};

const ACCENT: Color = Color::Cyan;
const WARNING: Color = Color::Yellow;
const MUTED: Color = Color::DarkGray;

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
    ConfirmSwitch { target: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchAction {
    ContinueHandoff(String),
    ResumeSession(String),
}

#[derive(Clone, Debug)]
struct UiState {
    screen: Screen,
    previous_screen: Screen,
    status: StatusView,
    profiles: Vec<Profile>,
    agents: Vec<CodingAgent>,
    workspaces: Vec<Workspace>,
    sessions: Vec<Session>,
    checkpoints: Vec<Checkpoint>,
    handoffs: Vec<HandoffRecord>,
    usage: UsageSummary,
    doctor: DoctorReport,
    selected: usize,
    agent_filter: AgentFilter,
    input: InputMode,
    message: Option<String>,
    launch_action: Option<LaunchAction>,
    refresh_requested: bool,
    refreshing: bool,
    ascii: bool,
}

impl UiState {
    fn load(app: &Application, ascii: bool) -> Result<Self> {
        let path = app
            .store
            .active_workspace()?
            .map_or_else(|| Path::new(".").to_path_buf(), |workspace| workspace.path);
        let active_profile = app.store.active_profile()?;
        let active_agent = active_profile
            .as_ref()
            .map(|profile| (profile.agent_id.as_str(), profile.agent_home.as_path()));
        let agent_health = app.agents.detect_all(active_agent);
        let agents = load_agents(app, &agent_health);
        let status_health =
            active_profile
                .as_ref()
                .map_or_else(unconfigured_agent_health, |profile| {
                    app.agents
                        .implemented()
                        .zip(&agent_health)
                        .find(|(adapter, _)| adapter.id() == profile.agent_id)
                        .map_or_else(unconfigured_agent_health, |(adapter, health)| {
                            health_for_adapter(adapter, health)
                        })
                });
        let status = app.status_with_agent(&path, status_health)?;
        let profiles = app.store.list_profiles()?;
        let screen = if profiles.is_empty() {
            Screen::Onboarding
        } else {
            Screen::Home
        };
        Ok(Self {
            screen,
            previous_screen: Screen::Home,
            status,
            profiles,
            agents,
            workspaces: app.store.list_workspaces()?,
            sessions: app.store.list_sessions(false)?,
            checkpoints: app.checkpoints().list()?,
            handoffs: app.handoffs().list()?,
            usage: app.store.usage_summary()?,
            doctor: run_doctor_with_agent_results(
                &app.paths,
                &app.config,
                &app.store,
                &app.git,
                &app.agents,
                false,
                &agent_health,
            ),
            selected: 0,
            agent_filter: AgentFilter::All,
            input: InputMode::Normal,
            message: None,
            launch_action: None,
            refresh_requested: false,
            refreshing: false,
            ascii,
        })
    }

    fn refresh(&mut self, app: &Application) -> Result<()> {
        let path = app.store.active_workspace()?.map_or_else(
            || self.status.workspace.path.clone(),
            |workspace| workspace.path,
        );
        let active_profile = app.store.active_profile()?;
        let status_health =
            active_profile
                .as_ref()
                .map_or_else(unconfigured_agent_health, |profile| {
                    self.agents
                        .iter()
                        .find(|agent| agent.id == profile.agent_id)
                        .map_or_else(
                            || crate::model::AgentHealth {
                                agent_id: profile.agent_id.clone(),
                                installed: false,
                                executable: None,
                                version: None,
                                auth_state: AuthState::Unknown,
                                message: "awaiting background provider refresh".into(),
                            },
                            coding_agent_health,
                        )
                });
        self.status = app.status_with_agent(&path, status_health)?;
        self.profiles = app.store.list_profiles()?;
        self.workspaces = app.store.list_workspaces()?;
        self.sessions = app.store.list_sessions(false)?;
        self.checkpoints = app.checkpoints().list()?;
        self.handoffs = app.handoffs().list()?;
        self.usage = app.store.usage_summary()?;
        self.selected = self.selected.min(self.current_len().saturating_sub(1));
        self.refresh_requested = true;
        Ok(())
    }

    fn apply_background_refresh(&mut self, mut refreshed: Self) {
        let message = self.message.clone();
        refreshed.screen = self.screen;
        refreshed.previous_screen = self.previous_screen;
        refreshed.agent_filter = self.agent_filter;
        refreshed.input = self.input.clone();
        refreshed.launch_action.clone_from(&self.launch_action);
        refreshed.selected = self.selected.min(refreshed.current_len().saturating_sub(1));
        refreshed.refresh_requested = self.refresh_requested;
        refreshed.refreshing = false;
        refreshed.message = match message {
            Some(message) if !message.starts_with("Checking local and provider") => Some(message),
            _ => Some("Refreshed local and provider state.".into()),
        };
        *self = refreshed;
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
        self.message = None;
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
        self.launch_action = Some(LaunchAction::ContinueHandoff(handoff.id.to_string()));
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
        self.launch_action = Some(LaunchAction::ResumeSession(session.id.to_string()));
        true
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

fn first_detected_agent(state: &UiState) -> usize {
    state
        .agents
        .iter()
        .position(|agent| agent.detected_version.is_some())
        .unwrap_or(0)
}

pub fn run(app: Application, ascii: bool) -> Result<()> {
    enable_raw_mode().map_err(terminal_error)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(terminal_error)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(terminal_error)?;
    terminal.clear().map_err(terminal_error)?;

    let result = run_loop(&mut terminal, &app, ascii);

    disable_raw_mode().map_err(terminal_error)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(terminal_error)?;
    terminal.show_cursor().map_err(terminal_error)?;
    match result? {
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

fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &Application,
    ascii: bool,
) -> Result<Option<LaunchAction>> {
    let Some(mut state) = load_state_responsively(terminal, app, ascii)? else {
        return Ok(None);
    };
    let mut refresh_worker: Option<UiStateLoader> = None;
    let mut exit_after_refresh = false;
    loop {
        if let Some(loaded) = poll_state_loader(&mut refresh_worker)? {
            state.refreshing = false;
            if exit_after_refresh {
                break;
            }
            match loaded {
                Ok(refreshed) => state.apply_background_refresh(refreshed),
                Err(error) => state.message = Some(format!("Refresh failed: {error}")),
            }
        }
        if state.refresh_requested && refresh_worker.is_none() {
            refresh_worker = Some(UiStateLoader::start(app, ascii)?);
            state.refresh_requested = false;
            state.refreshing = true;
        }
        terminal
            .draw(|frame| render(frame, &state))
            .map_err(|error| {
                ContextWakeError::InvalidData(format!("terminal draw failed: {error}"))
            })?;
        if !event::poll(Duration::from_millis(250)).map_err(terminal_error)? {
            continue;
        }
        if let Event::Key(key) = event::read().map_err(terminal_error)?
            && key.kind == event::KeyEventKind::Press
            && handle_key(app, &mut state, key)?
        {
            if refresh_worker.is_some() {
                exit_after_refresh = true;
                state.message = Some("Finishing bounded provider probes before exit…".into());
            } else {
                break;
            }
        }
    }
    Ok(state.launch_action)
}

struct UiStateLoader {
    receiver: mpsc::Receiver<Result<UiState>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl UiStateLoader {
    fn start(app: &Application, ascii: bool) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let loader_app = app.clone();
        let worker = thread::Builder::new()
            .name("contextwake-tui-load".into())
            .spawn(move || {
                let _ = sender.send(UiState::load(&loader_app, ascii));
            })
            .map_err(|error| {
                ContextWakeError::InvalidData(format!("could not start TUI state loader: {error}"))
            })?;
        Ok(Self {
            receiver,
            worker: Some(worker),
        })
    }

    fn join(mut self) -> thread::Result<()> {
        self.worker.take().expect("state loader worker").join()
    }
}

impl Drop for UiStateLoader {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn poll_state_loader(task: &mut Option<UiStateLoader>) -> Result<Option<Result<UiState>>> {
    let Some(active) = task.as_ref() else {
        return Ok(None);
    };
    match active.receiver.try_recv() {
        Ok(result) => {
            let completed = task.take().expect("active state loader");
            completed.join().map_err(|_| {
                ContextWakeError::InvalidData("TUI state loader worker panicked".into())
            })?;
            Ok(Some(result))
        }
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => {
            let completed = task.take().expect("disconnected state loader");
            let _ = completed.join();
            Err(ContextWakeError::InvalidData(
                "TUI state loader stopped unexpectedly".into(),
            ))
        }
    }
}

fn load_state_responsively<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &Application,
    ascii: bool,
) -> Result<Option<UiState>> {
    let mut load_task = Some(UiStateLoader::start(app, ascii)?);
    let mut frame_number = 0usize;
    let mut quit_requested = false;
    let load_result = loop {
        if let Some(result) = poll_state_loader(&mut load_task)? {
            break result;
        }
        terminal
            .draw(|frame| render_loading(frame, ascii, frame_number, quit_requested))
            .map_err(|error| {
                ContextWakeError::InvalidData(format!("terminal draw failed: {error}"))
            })?;
        frame_number = frame_number.wrapping_add(1);
        if event::poll(Duration::from_millis(100)).map_err(terminal_error)?
            && let Event::Key(key) = event::read().map_err(terminal_error)?
            && key.kind == event::KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc)
        {
            quit_requested = true;
        }
    };
    if quit_requested {
        Ok(None)
    } else {
        load_result.map(Some)
    }
}

fn render_loading(
    frame: &mut ratatui::Frame<'_>,
    ascii: bool,
    frame_number: usize,
    quit_requested: bool,
) {
    let spinner = if ascii {
        ["|", "/", "-", "\\"][frame_number % 4]
    } else {
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][frame_number % 10]
    };
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
            Line::from("Q/Esc exits safely after active probes are reaped."),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().title(" LOADING ").borders(Borders::ALL)),
        area,
    );
}

fn handle_key(app: &Application, state: &mut UiState, key: KeyEvent) -> Result<bool> {
    match &mut state.input {
        InputMode::AddProfile { value, agent_index } => {
            match key.code {
                KeyCode::Esc => state.input = InputMode::Normal,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Tab => {
                    if !state.agents.is_empty() {
                        *agent_index = (*agent_index + 1) % state.agents.len();
                    }
                }
                KeyCode::Enter => {
                    let name = value.trim().to_string();
                    if name.is_empty() {
                        state.message = Some("Profile name cannot be empty.".into());
                    } else if let Some(agent) = state.agents.get(*agent_index) {
                        let profile = app.create_profile_for_agent(
                            &name,
                            Some(&name),
                            None,
                            &agent.id,
                            None,
                            None,
                        )?;
                        state.input = InputMode::Normal;
                        state.screen = Screen::Profiles;
                        state.message = Some(format!(
                            "Created {} for {}. Authenticate with: ctx profile login {}",
                            profile.display_name, agent.display_name, profile.name
                        ));
                        state.refresh(app)?;
                    } else {
                        state.message = Some("No coding-agent adapter is available.".into());
                    }
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && value.chars().count() < 64 =>
                {
                    value.push(character);
                }
                _ => {}
            }
            return Ok(false);
        }
        InputMode::CreateCheckpoint { value } => {
            match key.code {
                KeyCode::Esc => state.input = InputMode::Normal,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Enter => {
                    let objective = value.trim().to_string();
                    if objective.is_empty() {
                        state.message = Some("Checkpoint objective cannot be empty.".into());
                    } else {
                        let workspace = app.resolve_workspace(None)?;
                        let profile = app.store.active_profile()?;
                        let checkpoint = app.checkpoints().create(
                            &workspace,
                            profile.as_ref(),
                            None,
                            CheckpointInput {
                                objective,
                                ..CheckpointInput::default()
                            },
                        )?;
                        state.input = InputMode::Normal;
                        state.message = Some(format!("Created checkpoint {}.", checkpoint.id));
                        state.refresh(app)?;
                    }
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && value.chars().count() < 200 =>
                {
                    value.push(character);
                }
                _ => {}
            }
            return Ok(false);
        }
        InputMode::SearchSessions { value } => {
            match key.code {
                KeyCode::Esc => state.input = InputMode::Normal,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Enter => {
                    let query = value.trim().to_lowercase();
                    state.input = InputMode::Normal;
                    if query.is_empty() {
                        state.refresh(app)?;
                        state.message = Some("Session search cleared.".into());
                    } else {
                        state
                            .sessions
                            .retain(|session| session_matches(session, &query));
                        state.selected = 0;
                        state.message = Some(format!(
                            "{} matching local session(s). Press R to clear.",
                            state.sessions.len()
                        ));
                    }
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && value.chars().count() < 128 =>
                {
                    value.push(character);
                }
                _ => {}
            }
            return Ok(false);
        }
        InputMode::ConfirmSwitch { target } => {
            let target = *target;
            match key.code {
                KeyCode::Esc => state.input = InputMode::Normal,
                KeyCode::Enter => {
                    let profile = state.profiles.get(target).ok_or_else(|| {
                        ContextWakeError::InvalidData("selected profile disappeared".into())
                    })?;
                    let outcome = app.switch_active_profile(
                        &profile.id.to_string(),
                        Some(&state.status.workspace),
                        true,
                        None,
                    )?;
                    let handoff_id = outcome.handoff.as_ref().map(|handoff| handoff.id);
                    state.input = InputMode::Normal;
                    state.message = Some(format!(
                        "{} Review the handoff, then press Enter to start a NEW session.",
                        outcome.message
                    ));
                    state.refresh(app)?;
                    if let Some(handoff_id) = handoff_id
                        && let Some(index) = state
                            .handoffs
                            .iter()
                            .position(|handoff| handoff.id == handoff_id)
                    {
                        state.previous_screen = Screen::Profiles;
                        state.screen = Screen::HandoffPreview;
                        state.selected = index;
                    }
                }
                KeyCode::Char('x' | 'X') => {
                    let profile = state.profiles.get(target).ok_or_else(|| {
                        ContextWakeError::InvalidData("selected profile disappeared".into())
                    })?;
                    let outcome =
                        app.switch_active_profile(&profile.id.to_string(), None, false, None)?;
                    state.input = InputMode::Normal;
                    state.message = Some(outcome.message);
                    state.refresh(app)?;
                }
                _ => {}
            }
            return Ok(false);
        }
        InputMode::Normal => {}
    }

    match key.code {
        KeyCode::Char('q' | 'Q') => return Ok(true),
        KeyCode::Char('?') => state.select_screen(Screen::Help),
        KeyCode::Char('g' | 'G') => state.select_screen(Screen::Home),
        KeyCode::Char('p' | 'P') => state.select_screen(Screen::Profiles),
        KeyCode::Char('i' | 'I') => state.select_screen(Screen::AgentCapabilities),
        KeyCode::Char('w' | 'W') => state.select_screen(Screen::Workspaces),
        KeyCode::Char('s' | 'S') => state.select_screen(Screen::Sessions),
        KeyCode::Char('c' | 'C') => state.select_screen(Screen::Checkpoints),
        KeyCode::Char('o' | 'O') => state.select_screen(Screen::Handoffs),
        KeyCode::Char('u' | 'U') => state.select_screen(Screen::Usage),
        KeyCode::Char('d' | 'D') => state.select_screen(Screen::Diagnostics),
        KeyCode::Char('t' | 'T') => state.select_screen(Screen::Settings),
        KeyCode::Char('r' | 'R') => {
            if state.refreshing || state.refresh_requested {
                state.message = Some("Provider discovery is already running…".into());
            } else {
                state.refresh_requested = true;
                state.message = Some("Checking local and provider state…".into());
            }
        }
        KeyCode::Char('f' | 'F') if state.screen == Screen::AgentCapabilities => {
            state.agent_filter = state.agent_filter.next();
            state.selected = 0;
            state.message = Some(format!("Agent filter: {}.", state.agent_filter.label()));
        }
        KeyCode::Esc => {
            if matches!(
                state.screen,
                Screen::SessionDetail
                    | Screen::CheckpointDetail
                    | Screen::HandoffPreview
                    | Screen::Help
                    | Screen::Authentication
            ) {
                state.screen = state.previous_screen;
            } else {
                state.screen = Screen::Home;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.selected = state.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let length = state.current_len();
            if length > 0 {
                state.selected = (state.selected + 1).min(length - 1);
            }
        }
        KeyCode::Char('a' | 'A')
            if matches!(state.screen, Screen::Profiles | Screen::Onboarding) =>
        {
            state.input = InputMode::AddProfile {
                value: String::new(),
                agent_index: first_detected_agent(state),
            };
        }
        KeyCode::Char('n' | 'N') if state.screen == Screen::Checkpoints => {
            state.input = InputMode::CreateCheckpoint {
                value: String::new(),
            };
        }
        KeyCode::Char('n' | 'N') if state.screen == Screen::Handoffs => {
            if let Some(checkpoint) = state
                .checkpoints
                .iter()
                .find(|checkpoint| checkpoint.workspace_id == state.status.workspace.id)
            {
                let handoff = app.handoffs().create(checkpoint, &state.status.workspace)?;
                state.message = Some(format!(
                    "Created handoff {}. Press Enter to preview.",
                    handoff.id
                ));
                state.refresh(app)?;
            } else {
                state.message = Some("Create a checkpoint before creating a handoff.".into());
            }
        }
        KeyCode::Char('/') if state.screen == Screen::Sessions => {
            state.input = InputMode::SearchSessions {
                value: String::new(),
            };
        }
        KeyCode::Char('l' | 'L') if state.screen == Screen::Profiles => {
            state.previous_screen = Screen::Profiles;
            state.screen = Screen::Authentication;
        }
        KeyCode::Enter => match state.screen {
            Screen::Profiles => {
                if let Some(profile) = state.profiles.get(state.selected) {
                    let active = state.status.active_profile.as_ref().map(|value| value.id);
                    if active == Some(profile.id) {
                        state.message =
                            Some(format!("{} is already active.", profile.display_name));
                    } else {
                        state.input = InputMode::ConfirmSwitch {
                            target: state.selected,
                        };
                    }
                }
            }
            Screen::Workspaces => {
                if let Some(workspace) = state.workspaces.get(state.selected) {
                    app.store.set_active_workspace(workspace.id)?;
                    state.message = Some(format!(
                        "Opened {}. No repository command was executed.",
                        workspace.display_name
                    ));
                    state.refresh(app)?;
                }
            }
            Screen::Sessions => {
                state.previous_screen = Screen::Sessions;
                state.screen = Screen::SessionDetail;
            }
            Screen::SessionDetail => {
                if state.queue_selected_session() {
                    return Ok(true);
                }
            }
            Screen::Checkpoints => {
                state.previous_screen = Screen::Checkpoints;
                state.screen = Screen::CheckpointDetail;
            }
            Screen::Handoffs => {
                state.previous_screen = Screen::Handoffs;
                state.screen = Screen::HandoffPreview;
            }
            Screen::HandoffPreview => {
                if state.queue_selected_handoff() {
                    return Ok(true);
                }
            }
            Screen::Onboarding => {
                state.input = InputMode::AddProfile {
                    value: "Personal".into(),
                    agent_index: first_detected_agent(state),
                };
            }
            _ => {}
        },
        _ => {}
    }
    Ok(false)
}

fn render(frame: &mut ratatui::Frame<'_>, state: &UiState) {
    let area = frame.area();
    if area.width < 42 || area.height < 12 {
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
}

fn render_header(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    let git = state.status.git.as_ref();
    let state_marker = if git.is_some_and(|value| value.dirty) {
        "[DIRTY]"
    } else {
        "[CLEAN]"
    };
    let branch = git
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
    spans.push(Span::styled(
        right,
        Style::default().fg(if state_marker == "[DIRTY]" {
            WARNING
        } else {
            Color::Green
        }),
    ));
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
    let git = state.status.git.as_ref();
    let git_lines = if let Some(git) = git {
        vec![
            Line::from(format!(
                "Repository  {}",
                git.repository_root
                    .as_ref()
                    .map_or_else(|| "unavailable".into(), |path| path.display().to_string())
            )),
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
    } else {
        vec![
            Line::from("[NON-GIT] Ordinary directory workspace"),
            Line::from("Git-only continuity fields are unavailable."),
        ]
    };
    frame.render_widget(
        Paragraph::new(git_lines)
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
                Line::from(format!("Continuity  {}", session.continuity.as_str())),
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
    frame.render_widget(
        Paragraph::new(continuity)
            .block(Block::default().title(" CONTINUITY ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        right[1],
    );
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
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        })
    });
    frame.render_widget(
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
    );
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
    let visible_agents = state
        .agents
        .iter()
        .filter(|agent| state.agent_filter.includes(agent))
        .collect::<Vec<_>>();
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
    let mut list_state = ListState::default().with_selected(Some(state.selected));
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
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        sections[0],
        &mut list_state,
    );
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
            Line::from(format!("Executable         {}", executable.display())),
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
        .enumerate()
        .map(|(index, workspace)| {
            let marker = if workspace.id == active {
                "[OPEN]"
            } else {
                "      "
            };
            let line = format!(
                "{} {:<24} {}",
                marker,
                workspace.display_name,
                workspace.path.display()
            );
            ListItem::new(line).style(if index == state.selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" WORKSPACES · Enter open · repository commands never auto-run ")
                .borders(Borders::ALL),
        ),
        area,
    );
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
        .enumerate()
        .map(|(index, session)| {
            let line = format!(
                "{} {} · {} · {} · {}",
                if index == state.selected { ">" } else { " " },
                session.title.as_deref().unwrap_or("Untitled session"),
                session
                    .provider_session_id
                    .as_deref()
                    .unwrap_or("provider id unavailable"),
                session.agent_id,
                session.continuity.as_str()
            );
            ListItem::new(line).style(if index == state.selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" RECENT SESSIONS · Enter details · / search ")
                .borders(Borders::ALL),
        ),
        area,
    );
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
            Line::from(format!("Continuity     {}", session.continuity.as_str())),
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
        .enumerate()
        .map(|(index, checkpoint)| {
            ListItem::new(format!(
                "{} {} · {} · {}",
                if index == state.selected { ">" } else { " " },
                checkpoint.created_at.format("%Y-%m-%d %H:%M"),
                checkpoint.redaction_status.as_str(),
                checkpoint.objective
            ))
            .style(if index == state.selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" CHECKPOINTS · N create ")
                .borders(Borders::ALL),
        ),
        area,
    );
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
        .enumerate()
        .map(|(index, handoff)| {
            ListItem::new(format!(
                "{} {} · schema {} · checkpoint {}",
                if index == state.selected { ">" } else { " " },
                handoff.created_at.format("%Y-%m-%d %H:%M"),
                handoff.schema_version,
                handoff.checkpoint_id
            ))
            .style(if index == state.selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" HANDOFFS · N create from latest checkpoint · Enter preview ")
                .borders(Borders::ALL),
        ),
        area,
    );
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
    let launch_instruction = if handoff.checkpoint_id.is_nil() {
        format!(
            "Imported package: use `ctx handoff continue {} --workspace <path>`",
            handoff.id
        )
    } else {
        "[Enter] Start a NEW agent session from this handoff".into()
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "[INSPECT BEFORE EXPORT]",
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Handoff      {}", handoff.id)),
        Line::from(format!("Schema       {}", handoff.schema_version)),
        Line::from(format!("Integrity    {}", handoff.sha256)),
        Line::from(""),
        Line::from("This package contains explicit project state only."),
        Line::from("Commands are untrusted notes and are never auto-executed."),
        Line::from(""),
        Line::from(Span::styled(
            launch_instruction,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("[Esc] Return without launching a provider"),
    ];
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
    let rows = state.doctor.checks.iter().map(|check| {
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
    });
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
            .alignment(Alignment::Center),
        area,
    );
}

fn render_overlay(frame: &mut ratatui::Frame<'_>, state: &UiState, area: Rect) {
    match &state.input {
        InputMode::Normal => {}
        InputMode::AddProfile { value, agent_index } => {
            let popup = centered(area, 62, 10);
            let agent = state
                .agents
                .get(*agent_index)
                .map_or("unavailable", |agent| agent.display_name.as_str());
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
            let Some(profile) = state.profiles.get(*target) else {
                return;
            };
            let git = state.status.git.as_ref();
            let popup = centered(area, 76, 15);
            frame.render_widget(Clear, popup);
            let changes = git.map_or_else(
                || "Non-Git workspace".into(),
                |git| {
                    format!(
                        "{} staged · {} unstaged · {} untracked · +{} -{}",
                        git.staged_count,
                        git.unstaged_count,
                        git.untracked_count,
                        git.additions,
                        git.deletions
                    )
                },
            );
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
                    Line::from(""),
                    Line::from("[Enter] Create checkpoint + handoff, then switch"),
                    Line::from("[X]     Switch without a continuity artifact"),
                    Line::from("[Esc]   Cancel"),
                ])
                .block(
                    Block::default()
                        .title(" CONFIRM IDENTITY CHANGE ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(WARNING)),
                )
                .wrap(Wrap { trim: true }),
                popup,
            );
        }
    }
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
    let git =
        state.status.git.as_ref().map_or(
            "no-git",
            |value| if value.dirty { "dirty" } else { "clean" },
        );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                PRODUCT_NAME,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("Profile: {profile}")),
            Line::from(format!(
                "Workspace: {}",
                state.status.workspace.display_name
            )),
            Line::from(format!("Git: {git}")),
            Line::from(""),
            Line::from("Terminal is narrow."),
            Line::from("Q quit · ? help"),
        ])
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
    use std::path::PathBuf;

    use chrono::Utc;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use uuid::Uuid;

    use super::*;
    use crate::doctor::{CheckStatus, DoctorCheck};
    use crate::model::{AgentHealth, AuthState, GitSnapshot, RedactionStatus, TrustState};
    use crate::provider::{AgentAdapter, CodexAdapter};

    fn fixture() -> UiState {
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: PathBuf::from("C:/very/long/workspace/path/example"),
            display_name: "Example Workspace".into(),
            trust_state: TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: Some(PathBuf::from("C:/very/long/workspace/path/example")),
            created_at: now,
            last_opened_at: now,
        };
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "work".into(),
            display_name: "Work".into(),
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model_preference: None,
            description: None,
            agent_home: PathBuf::from("C:/state/codex/work"),
            account_fingerprint: None,
            auth_state: AuthState::SignedIn,
            created_at: now,
            updated_at: now,
            last_used_at: Some(now),
        };
        let workspace_id = workspace.id;
        UiState {
            screen: Screen::Home,
            previous_screen: Screen::Home,
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
            profiles: vec![profile],
            agents: vec![CodingAgent {
                id: "codex".into(),
                display_name: "OpenAI Codex CLI".into(),
                adapter_version: VERSION.into(),
                detected_version: Some("codex-cli 0.154.0".into()),
                executable: Some(PathBuf::from("codex")),
                auth_state: crate::model::AuthState::SignedIn,
                capabilities: CodexAdapter::with_executable("codex").capabilities(),
            }],
            workspaces: vec![workspace],
            sessions: Vec::new(),
            checkpoints: vec![Checkpoint {
                schema_version: 2,
                id: Uuid::new_v4(),
                workspace_id,
                session_id: None,
                agent_id: Some("codex".into()),
                model_provider_id: Some("openai".into()),
                model: None,
                profile_id: None,
                objective: "Continue context continuity implementation".into(),
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
            }],
            handoffs: Vec::new(),
            usage: UsageSummary {
                provider_usage_available: false,
                provider_usage_message: "Unavailable".into(),
                local_session_count: 0,
                local_checkpoint_count: 1,
                local_handoff_count: 0,
                local_profile_switch_count: 1,
            },
            doctor: DoctorReport {
                application: PRODUCT_NAME.into(),
                version: VERSION.into(),
                checks: vec![DoctorCheck {
                    name: "Application".into(),
                    status: CheckStatus::Pass,
                    summary: "ok".into(),
                    action: None,
                }],
            },
            selected: 0,
            agent_filter: AgentFilter::All,
            input: InputMode::Normal,
            message: None,
            launch_action: None,
            refresh_requested: false,
            refreshing: false,
            ascii: false,
        }
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
        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4(),
            provider_session_id: Some("feature-inspection".into()),
            title: Some("Inspection workflow".into()),
            workspace_id: Uuid::new_v4(),
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

        let mut state = fixture();
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
}
