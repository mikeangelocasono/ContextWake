use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Clone, Debug, Parser)]
#[command(
    name = "adeck",
    version,
    about = "Terminal-native identity and continuity manager for AI coding CLIs",
    long_about = None
)]
pub struct Cli {
    /// Emit stable machine-readable JSON where supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Use ASCII-only status markers in human-readable output.
    #[arg(long, global = true)]
    pub ascii: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Show active profile, coding agent, model backend, workspace, and Git state.
    Status(StatusArgs),
    /// Run privacy-safe local and provider diagnostics.
    Doctor(DoctorArgs),
    /// Detect coding agents and inspect their capability contracts.
    #[command(alias = "provider")]
    Agent(AgentArgs),
    /// Manage local identity metadata and provider-owned auth boundaries.
    Profile(ProfileArgs),
    /// Register and select repositories or ordinary directories.
    Workspace(WorkspaceArgs),
    /// Browse local session metadata and use provider-native resume.
    Session(SessionArgs),
    /// Create and inspect durable local project-state snapshots.
    Checkpoint(CheckpointArgs),
    /// Create, preview, export, and continue from portable handoffs.
    Handoff(HandoffArgs),
    /// Show provider-reported availability separately from local activity.
    Usage,
    /// Inspect or select a profile's model preference.
    Model(ModelArgs),
    /// Inspect and validate `AgentDeck` configuration.
    Config(ConfigArgs),
    /// Generate completion scripts.
    Completion(CompletionArgs),
    /// Print application version.
    Version,
}

#[derive(Clone, Debug, Args)]
pub struct StatusArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub struct DoctorArgs {
    /// Include safe executable paths and capability details; secrets stay redacted.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub command: ProfileCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ProfileCommand {
    Add {
        name: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, alias = "provider", default_value = "codex")]
        agent: String,
        #[arg(long)]
        model_provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    List,
    Show {
        profile: String,
    },
    Use {
        profile: String,
        /// Create a checkpoint and handoff before activating the target profile.
        #[arg(long)]
        handoff: bool,
        #[arg(long)]
        objective: Option<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Remove {
        profile: String,
        /// Confirm metadata removal. Provider credentials are left untouched.
        #[arg(long)]
        yes: bool,
    },
    Login {
        profile: String,
        #[arg(long)]
        device_auth: bool,
    },
    Logout {
        profile: String,
        /// Confirm provider-owned credential removal.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum WorkspaceCommand {
    Add {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Trust metadata and argv-only validation declarations in project.toml.
        #[arg(long)]
        trust: bool,
    },
    List,
    Show {
        workspace: String,
    },
    Open {
        workspace: String,
    },
    Remove {
        workspace: String,
        #[arg(long)]
        yes: bool,
    },
    /// Run trusted project validation commands after an explicit user action.
    Validate {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum SessionCommand {
    /// Discover supported provider sessions and index those for one workspace.
    Sync {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, default_value_t = 100, value_parser = parse_session_count)]
        max_count: usize,
    },
    List {
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, alias = "provider")]
        agent: Option<String>,
        #[arg(long)]
        query: Option<String>,
    },
    Show {
        session: String,
    },
    Resume {
        /// `AgentDeck` session UUID or provider session UUID/name.
        session: String,
    },
    Archive {
        session: String,
        #[arg(long)]
        yes: bool,
    },
}

fn parse_session_count(value: &str) -> std::result::Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|_| "session count must be an integer".to_string())?;
    if (1..=500).contains(&count) {
        Ok(count)
    } else {
        Err("session count must be between 1 and 500".into())
    }
}

#[derive(Clone, Debug, Args)]
pub struct CheckpointArgs {
    #[command(subcommand)]
    pub command: CheckpointCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum CheckpointCommand {
    Create {
        #[arg(long)]
        objective: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long = "completed")]
        completed: Vec<String>,
        #[arg(long = "decision")]
        decisions: Vec<String>,
        #[arg(long = "pending")]
        pending: Vec<String>,
        #[arg(long = "known-issue")]
        known_issues: Vec<String>,
        /// Explicitly run trusted project validation commands and capture their statuses.
        #[arg(long)]
        validate: bool,
    },
    List,
    Show {
        checkpoint: String,
    },
    Delete {
        checkpoint: String,
        #[arg(long)]
        yes: bool,
    },
    Export {
        checkpoint: String,
        destination: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub struct HandoffArgs {
    #[command(subcommand)]
    pub command: HandoffCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum HandoffCommand {
    Create {
        checkpoint: String,
    },
    List,
    Show {
        handoff: String,
    },
    Preview {
        handoff: String,
    },
    Export {
        handoff: String,
        destination: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Import and verify an untrusted handoff directory.
    Import {
        path: PathBuf,
    },
    /// Start a new provider session using an explicit handoff.
    Continue {
        handoff: String,
        /// Required for imported handoffs without a local checkpoint association.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum AgentCommand {
    List,
    Detect,
    Info { agent: String },
}

#[derive(Clone, Debug, Args)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: ModelCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ModelCommand {
    /// Show the active profile's configured model and verified backend choices.
    List,
    /// Set a model preference after validating that the coding agent supports selection.
    Select {
        model: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

#[derive(Clone, Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
    Path,
    Validate,
    /// Atomically update non-secret global settings.
    Set {
        #[arg(long)]
        telemetry: Option<bool>,
        #[arg(long)]
        retention_days: Option<u32>,
        #[arg(long)]
        ascii: Option<bool>,
        #[arg(long)]
        update_check: Option<bool>,
        #[arg(long)]
        git_timeout_ms: Option<u64>,
        #[arg(long)]
        validation_timeout_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}
