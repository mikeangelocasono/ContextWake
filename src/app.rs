use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use clap::CommandFactory;
use serde::Serialize;
use uuid::Uuid;

use crate::checkpoint::{CheckpointInput, CheckpointService};
use crate::cli::{
    AgentCommand, CheckpointCommand, Cli, Command, ConfigCommand, HandoffCommand, ModelCommand,
    ProfileCommand, SessionCommand, WorkspaceCommand,
};
use crate::config::AppConfig;
use crate::continuity::{SwitchOutcome, switch_profile};
use crate::doctor::{CheckStatus, run_doctor};
use crate::error::{ContextWakeError, Result};
use crate::git::GitClient;
use crate::guardian::assess;
use crate::handoff::HandoffService;
use crate::model::{
    AuthState, ContinuityKind, Profile, ResumeCapability, Session, TrustState, Workspace,
};
use crate::paths::AppPaths;
use crate::provider::AgentRegistry;
use crate::security::validate_external_reference;
use crate::security::{
    profile_slug, sanitize_terminal, sanitize_untrusted_label, validate_local_name,
};
use crate::store::Store;
use crate::validation::ValidationRunner;
use crate::workspace::{detect_workspace, load_project_config};
use crate::{BINARY_NAME, PRODUCT_NAME, VERSION};

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
                let status = self.status(&args.path)?;
                print_status(&status, cli.json, cli.ascii);
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
                if cli.json {
                    print_json(&report);
                } else {
                    println!("{PRODUCT_NAME} Doctor\n");
                    for check in &report.checks {
                        let status_marker = marker(check.status, cli.ascii);
                        println!("{status_marker:<7} {:<22} {}", check.name, check.summary);
                        if let Some(action) = &check.action {
                            println!("        Action: {action}");
                        }
                    }
                }
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
                match args.command {
                    ConfigCommand::Show => print_json(&self.config),
                    ConfigCommand::Path => println!("{}", self.paths.config_file().display()),
                    ConfigCommand::Validate => {
                        self.config.validate()?;
                        println!(
                            "Configuration is valid: {}",
                            self.paths.config_file().display()
                        );
                    }
                    ConfigCommand::Set {
                        telemetry,
                        retention_days,
                        ascii,
                        update_check,
                        git_timeout_ms,
                        validation_timeout_ms,
                    } => {
                        if telemetry.is_none()
                            && retention_days.is_none()
                            && ascii.is_none()
                            && update_check.is_none()
                            && git_timeout_ms.is_none()
                            && validation_timeout_ms.is_none()
                        {
                            return Err(ContextWakeError::Configuration(
                                "config set requires at least one setting".into(),
                            ));
                        }
                        let mut config = self.config.clone();
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
                        if let Some(value) = validation_timeout_ms {
                            config.validation_timeout_ms = value;
                        }
                        config.save(&self.paths)?;
                        if cli.json {
                            print_json(&config);
                        } else {
                            println!(
                                "Updated non-secret configuration at {}.",
                                self.paths.config_file().display()
                            );
                        }
                    }
                }
                Ok(0)
            }
            Some(Command::Completion(args)) => {
                let mut command = Cli::command();
                let binary_name = command.get_name().to_string();
                clap_complete::generate(args.shell, &mut command, binary_name, &mut io::stdout());
                Ok(0)
            }
            Some(Command::Version) => {
                if cli.json {
                    print_json(
                        &serde_json::json!({"application": PRODUCT_NAME, "version": VERSION}),
                    );
                } else {
                    println!("{BINARY_NAME} {VERSION}");
                }
                Ok(0)
            }
        }
    }

    pub fn status(&self, path: &Path) -> Result<StatusView> {
        let active_profile = self.store.active_profile()?;
        let agent = if let Some(profile) = &active_profile {
            self.agents
                .get(&profile.agent_id)?
                .detect(Some(&profile.agent_home))?
        } else {
            crate::model::AgentHealth {
                agent_id: "unconfigured".into(),
                installed: false,
                executable: None,
                version: None,
                auth_state: AuthState::Unknown,
                message: "no active coding-agent profile".into(),
            }
        };
        self.status_with_agent(path, agent)
    }

    pub(crate) fn status_with_agent(
        &self,
        path: &Path,
        agent: crate::model::AgentHealth,
    ) -> Result<StatusView> {
        let workspace = self.register_workspace(path, TrustState::Untrusted, false)?;
        let mut active_profile = self.store.active_profile()?;
        if let Some(profile) = active_profile.as_mut() {
            let previous_auth_state = profile.auth_state;
            profile.auth_state = agent.auth_state;
            if agent.installed && previous_auth_state != agent.auth_state {
                self.store
                    .update_profile_auth(profile.id, agent.auth_state)?;
            }
        }
        let git = self.git.snapshot(&workspace.path)?;
        let changed_files = self.git.changed_files(&workspace.path)?;
        let latest_checkpoint = self.checkpoints().latest_for_workspace(workspace.id)?;
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
        Ok(StatusView {
            application: PRODUCT_NAME.into(),
            version: VERSION.into(),
            active_profile,
            workspace,
            git,
            agent,
            latest_session,
            context_information:
                "Unavailable from stable provider interfaces; local continuity remains available"
                    .into(),
            continuity_guardian,
        })
    }

    pub fn register_workspace(
        &self,
        path: &Path,
        trust: TrustState,
        activate: bool,
    ) -> Result<Workspace> {
        let mut workspace = detect_workspace(path, &self.git, trust)?;
        if let Some(existing) = self.store.workspace_by_path(&workspace.path)? {
            workspace.id = existing.id;
            workspace.created_at = existing.created_at;
            if existing.trust_state == TrustState::Trusted {
                workspace.trust_state = TrustState::Trusted;
            }
        }
        if workspace.trust_state == TrustState::Trusted
            && let Some(project) = load_project_config(&workspace)?
        {
            if let Some(name) = project.name {
                workspace.display_name = name;
            }
            workspace.preferred_agent_id = project.preferred_agent;
            workspace.preferred_model_provider_id = project.preferred_model_provider;
            workspace.preferred_model = project.preferred_model;
        }
        self.store.upsert_workspace(&workspace)?;
        if activate {
            self.store.set_active_workspace(workspace.id)?;
        }
        Ok(workspace)
    }

    pub fn resolve_workspace(&self, path: Option<&Path>) -> Result<Workspace> {
        if let Some(path) = path {
            return self.register_workspace(path, TrustState::Untrusted, true);
        }
        if let Some(workspace) = self.store.active_workspace()? {
            return Ok(workspace);
        }
        self.register_workspace(Path::new("."), TrustState::Untrusted, true)
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
        let requested_name = validate_local_name(name)?;
        let name = profile_slug(&requested_name)?;
        let display_name = validate_local_name(display_name.unwrap_or(&requested_name))?;
        let requested_agent = validate_external_reference(agent_id)?.to_ascii_lowercase();
        let adapter = self.agents.get(&requested_agent)?;
        let agent_id = adapter.id().to_string();
        let mut model_provider_id = model_provider_id
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
        let mut model_preference = model_preference
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
        let id = Uuid::new_v4();
        let agent_home = self.paths.agent_home(&agent_id, &id);
        adapter.initialize_profile_home(&agent_home)?;
        let now = Utc::now();
        let profile = Profile {
            id,
            name,
            display_name,
            agent_id,
            model_provider_id,
            model_preference,
            description: description.map(sanitize_terminal),
            agent_home,
            account_fingerprint: None,
            auth_state: AuthState::SignedOut,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        if let Err(error) = self.store.insert_profile(&profile) {
            // This UUID-scoped home was created by this operation and has not
            // been exposed to an interactive agent process yet.
            let _ = std::fs::remove_dir_all(&profile.agent_home);
            return Err(error);
        }
        let became_active = self.store.active_profile()?.is_none();
        if became_active {
            self.store.set_active_profile(profile.id)?;
            return self.store.profile(&profile.id.to_string());
        }
        Ok(profile)
    }

    pub fn switch_active_profile(
        &self,
        target_reference: &str,
        workspace: Option<&Workspace>,
        create_handoff: bool,
        objective: Option<String>,
    ) -> Result<SwitchOutcome> {
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
        switch_profile(
            &self.store,
            &self.checkpoints(),
            &self.handoffs(),
            target_reference,
            workspace,
            create_handoff,
            objective,
        )
    }

    pub fn continue_handoff(&self, handoff: &str, workspace_path: Option<&Path>) -> Result<()> {
        let service = self.handoffs();
        let checkpoints = self.checkpoints();
        let record = self.store.handoff(handoff)?;
        let workspace = match checkpoints.read(&record.checkpoint_id.to_string()) {
            Ok(checkpoint) => self.store.workspace(&checkpoint.workspace_id.to_string())?,
            Err(ContextWakeError::CheckpointNotFound(_)) => {
                let path = workspace_path.ok_or_else(|| {
                    ContextWakeError::InvalidData(
                        "this imported handoff has no local checkpoint; pass --workspace".into(),
                    )
                })?;
                self.resolve_workspace(Some(path))?
            }
            Err(error) => return Err(error),
        };
        let profile = self
            .store
            .active_profile()?
            .ok_or_else(|| ContextWakeError::InvalidData("an active profile is required".into()))?;

        // Validate the manifest and every referenced content digest immediately before launch.
        let _ = service.validate(handoff)?;
        let adapter = self.agents.get(&profile.agent_id)?;
        adapter.start_with_handoff(
            &profile.agent_home,
            &workspace.path,
            &record.storage_path,
            profile.model_provider_id.as_deref(),
            profile.model_preference.as_deref(),
        )?;
        let now = Utc::now();
        self.store.insert_session(&Session {
            id: Uuid::new_v4(),
            provider_session_id: None,
            title: None,
            workspace_id: workspace.id,
            profile_id: Some(profile.id),
            agent_id: profile.agent_id.clone(),
            model_provider_id: profile.model_provider_id.clone(),
            model: profile.model_preference.clone(),
            started_at: now,
            last_seen_at: now,
            resume_capability: ResumeCapability::Unknown,
            archived: false,
            continuity: ContinuityKind::RestoredFromHandoff,
        })?;
        println!(
            "Restored from Handoff: started a new {} session using workspace handoff {handoff}. This was not a native session resume.",
            adapter.display_name()
        );
        Ok(())
    }

    pub fn resume_session(&self, session_reference: &str) -> Result<()> {
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
        if let Some(known) = &known {
            if known.agent_id != profile.agent_id {
                return Err(ContextWakeError::Provider(format!(
                    "This session belongs to agent {}, but active profile {} uses {}. Native cross-agent resume was not attempted. Create a handoff instead.",
                    known.agent_id, profile.display_name, profile.agent_id
                )));
            }
            if known.profile_id.is_some_and(|id| id != profile.id) {
                return Err(ContextWakeError::Provider(
                    "This session was recorded under another profile. Native cross-profile resume was not attempted. Create a handoff instead."
                        .into(),
                ));
            }
            if known.resume_capability != ResumeCapability::Native
                || known.provider_session_id.is_none()
            {
                return Err(ContextWakeError::CapabilityUnavailable(
                    "this local session has no verified native provider session ID; continue with a workspace handoff"
                        .into(),
                ));
            }
        }
        let workspace = if let Some(known) = &known {
            self.store.workspace(&known.workspace_id.to_string())?
        } else {
            self.resolve_workspace(None)?
        };
        let provider_session_id = known
            .as_ref()
            .and_then(|session| session.provider_session_id.as_deref())
            .unwrap_or(session_reference);
        let adapter = self.agents.get(&profile.agent_id)?;
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
                resume_capability: ResumeCapability::Native,
                archived: false,
                continuity: ContinuityKind::NativeResume,
            })?;
        }
        println!(
            "Native {} session resume completed.",
            adapter.display_name()
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
            } => {
                let profile = self.create_profile_for_agent(
                    &name,
                    display_name.as_deref(),
                    description.as_deref(),
                    &agent,
                    model_provider.as_deref(),
                    model.as_deref(),
                )?;
                if json {
                    print_json(&profile);
                } else {
                    println!(
                        "Created profile {} ({}).\nCredentials remain provider-owned. Sign in with:\n  ctx profile login {}",
                        profile.display_name, profile.agent_id, profile.name
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
                            "{active_marker} {:<20} {:<8} {}",
                            profile.display_name,
                            profile.agent_id,
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
                    println!("  Home:     {}", profile.agent_home.display());
                }
            }
            ProfileCommand::Use {
                profile,
                handoff,
                objective,
                workspace,
            } => {
                let workspace = if handoff {
                    Some(self.resolve_workspace(workspace.as_deref())?)
                } else {
                    None
                };
                let outcome =
                    self.switch_active_profile(&profile, workspace.as_ref(), handoff, objective)?;
                if json {
                    print_json(&serde_json::json!({
                        "previous_profile": outcome.previous,
                        "active_profile": outcome.active,
                        "handoff": outcome.handoff,
                        "message": outcome.message,
                    }));
                } else {
                    println!("Active profile: {}", outcome.active.display_name);
                    println!("{}", outcome.message);
                    if let Some(handoff) = outcome.handoff {
                        println!("Handoff: {}", handoff.id);
                    }
                }
            }
            ProfileCommand::Remove { profile, yes } => {
                require_confirmation(yes, "profile metadata removal", "--yes")?;
                let removed = self.store.remove_profile(&profile)?;
                println!(
                    "Removed ContextWake metadata for {}. Provider credentials and {} were not deleted.",
                    removed.display_name,
                    removed.agent_home.display()
                );
            }
            ProfileCommand::Login {
                profile,
                device_auth,
            } => {
                let profile = self.store.profile(&profile)?;
                let adapter = self.agents.get(&profile.agent_id)?;
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
                require_confirmation(yes, "provider credential removal", "--yes")?;
                let profile = self.store.profile(&profile)?;
                let adapter = self.agents.get(&profile.agent_id)?;
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
                let workspace = self.register_workspace(
                    &path,
                    if trust {
                        TrustState::Trusted
                    } else {
                        TrustState::Untrusted
                    },
                    false,
                )?;
                output_value(&workspace, json, || {
                    format!(
                        "Registered {} ({})",
                        workspace.display_name,
                        workspace.path.display()
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
                            workspace.display_name,
                            workspace.path.display()
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
                self.store.set_active_workspace(workspace.id)?;
                output_value(&workspace, json, || {
                    format!(
                        "Opened workspace {}. No provider or repository command was launched.",
                        workspace.display_name
                    )
                });
            }
            WorkspaceCommand::Remove { workspace, yes } => {
                require_confirmation(yes, "workspace metadata removal", "--yes")?;
                let workspace = self.store.remove_workspace(&workspace)?;
                println!(
                    "Removed workspace metadata for {}. Repository files were not changed.",
                    workspace.display_name
                );
            }
            WorkspaceCommand::Validate { workspace } => {
                let workspace = self.resolve_workspace(workspace.as_deref())?;
                let results = self.run_project_validation(&workspace)?;
                exit_code = i32::from(
                    results
                        .iter()
                        .any(|result| result.status != crate::model::ValidationStatus::Passed),
                );
                if json {
                    print_json(&results);
                } else {
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
                let workspace = self.resolve_workspace(workspace.as_deref())?;
                let adapter = self.agents.get(&profile.agent_id)?;
                let capabilities = adapter.capabilities();
                if !capabilities.session_listing.is_available() {
                    return Err(ContextWakeError::CapabilityUnavailable(format!(
                        "{} does not expose a supported session list",
                        adapter.display_name()
                    )));
                }
                let discovered =
                    adapter.list_sessions(&profile.agent_home, &workspace.path, max_count)?;
                let canonical_workspace =
                    workspace
                        .path
                        .canonicalize()
                        .map_err(|source| ContextWakeError::Io {
                            path: workspace.path.clone(),
                            source,
                        })?;
                let mut matched = 0_usize;
                let mut created = 0_usize;
                for item in discovered {
                    let belongs_to_workspace = item.workspace_path.as_ref().is_some_and(|path| {
                        path.canonicalize()
                            .is_ok_and(|candidate| candidate == canonical_workspace)
                    });
                    if !belongs_to_workspace {
                        continue;
                    }
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
                            crate::model::CapabilitySupport::Verified => ResumeCapability::Native,
                            crate::model::CapabilitySupport::Unsupported => {
                                ResumeCapability::HandoffOnly
                            }
                            crate::model::CapabilitySupport::Partial
                            | crate::model::CapabilitySupport::Experimental
                            | crate::model::CapabilitySupport::Unknown => ResumeCapability::Unknown,
                        },
                        archived: false,
                        continuity: ContinuityKind::Unknown,
                    };
                    created += usize::from(self.store.upsert_provider_session(&session)?);
                }
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
                        "Indexed {matched} {} session(s) for {}: {created} new, {} updated.",
                        adapter.display_name(),
                        workspace.display_name,
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
            SessionCommand::Resume { session } => {
                self.resume_session(&session)?;
            }
            SessionCommand::Archive { session, yes } => {
                require_confirmation(yes, "local session metadata archival", "--yes")?;
                let session = self.store.archive_session(&session, true)?;
                println!(
                    "Archived local metadata for {}. Provider session data was not deleted.",
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
            } => {
                let workspace = self.resolve_workspace(workspace.as_deref())?;
                let profile = self.store.active_profile()?;
                let validation_results = if validate {
                    self.run_project_validation(&workspace)?
                } else {
                    Vec::new()
                };
                let checkpoint = service.create(
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
                )?;
                output_value(&checkpoint, json, || {
                    format!(
                        "Created checkpoint {} (redaction: {})",
                        checkpoint.id,
                        checkpoint.redaction_status.as_str()
                    )
                });
            }
            CheckpointCommand::List => {
                let checkpoints = service.list()?;
                if json {
                    print_json(&checkpoints);
                } else if checkpoints.is_empty() {
                    println!(
                        "No checkpoints. Create one with 'ctx checkpoint create --objective ...'."
                    );
                } else {
                    for checkpoint in checkpoints {
                        println!(
                            "{}  {}  {}",
                            checkpoint.id, checkpoint.created_at, checkpoint.objective
                        );
                    }
                }
            }
            CheckpointCommand::Show { checkpoint } => {
                let checkpoint = service.read(&checkpoint)?;
                output_value(&checkpoint, json, || format_checkpoint(&checkpoint));
            }
            CheckpointCommand::Delete { checkpoint, yes } => {
                require_confirmation(yes, "checkpoint deletion", "--yes")?;
                service.delete(&checkpoint)?;
                println!(
                    "Deleted checkpoint {checkpoint}. This cannot be recovered by ContextWake."
                );
            }
            CheckpointCommand::Export {
                checkpoint,
                destination,
                force,
            } => {
                let path = service.export_json(&checkpoint, &destination, force)?;
                println!("Exported checkpoint to {}", path.display());
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
                        "Created handoff {}. Preview with 'ctx handoff preview {}'.",
                        handoff.id, handoff.id
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
                    path.display()
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
            HandoffCommand::Continue { handoff, workspace } => {
                self.continue_handoff(&handoff, workspace.as_deref())?;
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

fn print_status(status: &StatusView, json: bool, ascii: bool) {
    if json {
        print_json(status);
        return;
    }
    let profile = status
        .active_profile
        .as_ref()
        .map_or("Not configured", |value| value.display_name.as_str());
    println!("{PRODUCT_NAME}");
    println!("  Active profile  {profile}");
    println!(
        "  Agent           {} {}",
        status.agent.agent_id,
        status.agent.version.as_deref().unwrap_or("unavailable")
    );
    if let Some(profile) = &status.active_profile {
        println!(
            "  Model backend   {}",
            profile
                .model_provider_id
                .as_deref()
                .unwrap_or("agent default")
        );
        println!(
            "  Model           {}",
            profile.model_preference.as_deref().unwrap_or("not exposed")
        );
    }
    println!("  Workspace       {}", status.workspace.display_name);
    println!("  Path            {}", status.workspace.path.display());
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
    } else {
        println!("  Git             Not a Git workspace");
    }
    println!("  Context         {}", status.context_information);
    println!(
        "  Local guardian  {} - {}",
        status.continuity_guardian.level.as_str().to_uppercase(),
        status.continuity_guardian.recommendation
    );
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
