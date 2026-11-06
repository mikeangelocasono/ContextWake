use std::io::IsTerminal;
use std::time::Duration;

use serde::Serialize;

use crate::config::AppConfig;
use crate::error::ContextWakeError;
use crate::git::GitClient;
use crate::model::{AgentHealth, AuthState, ProfileHomeMode};
use crate::paths::{AppPaths, paths_equivalent};
use crate::provider::{AgentAdapter, AgentRegistry};
use crate::security::{SecretScanner, sanitize_terminal};
use crate::store::Store;
use crate::{PRODUCT_NAME, VERSION};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub summary: String,
    pub action: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub application: String,
    pub version: String,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
    }
}

/// One piece of local state as far as `ctx doctor` could load it.
enum Loaded<'a, T> {
    Ready(&'a T),
    Failed(String),
    NotChecked(&'static str),
}

/// What doctor could initialize. Every failed step becomes a check instead of
/// aborting the report.
struct LoadedState<'a> {
    paths: Option<&'a AppPaths>,
    directories_error: Option<String>,
    config: Loaded<'a, AppConfig>,
    store: Loaded<'a, Store>,
}

pub fn run_doctor(
    paths: &AppPaths,
    config: &AppConfig,
    store: &Store,
    git: &GitClient,
    agents: &AgentRegistry,
    verbose: bool,
) -> DoctorReport {
    let active_profile = store.active_profile().ok().flatten();
    let active_agent = active_profile
        .as_ref()
        .map(|profile| (profile.agent_id.as_str(), profile.agent_home.as_path()));
    let agent_health = agents.detect_all(active_agent);
    run_doctor_with_agent_results(paths, config, store, git, agents, verbose, &agent_health)
}

pub(crate) fn run_doctor_with_agent_results(
    paths: &AppPaths,
    config: &AppConfig,
    store: &Store,
    git: &GitClient,
    agents: &AgentRegistry,
    verbose: bool,
    agent_health: &[crate::error::Result<AgentHealth>],
) -> DoctorReport {
    let state = LoadedState {
        paths: Some(paths),
        directories_error: None,
        config: Loaded::Ready(config),
        store: Loaded::Ready(store),
    };
    build_report(&state, git, agents, verbose, agent_health)
}

/// Diagnostics for a process whose local state could not be initialized: an
/// invalid `config.toml`, a state database that cannot be opened or migrated
/// (for example one written by a newer build), or unusable state directories.
/// Each step is retried independently and its failure is reported as a
/// `Fail` check, so the `ctx doctor` advice printed with startup errors works.
pub fn run_doctor_without_state(verbose: bool) -> DoctorReport {
    let agents = AgentRegistry::discover();
    let default_git_timeout = AppConfig::default().git_timeout_ms;
    let paths = match AppPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            let git = GitClient::new(Duration::from_millis(default_git_timeout));
            let agent_health = agents.detect_all(None);
            let state = LoadedState {
                paths: None,
                directories_error: Some(safe_error_text(&error)),
                config: Loaded::NotChecked("the state directories are unavailable"),
                store: Loaded::NotChecked("the state directories are unavailable"),
            };
            return build_report(&state, &git, &agents, verbose, &agent_health);
        }
    };
    let directories_error = paths.ensure().err().map(|error| safe_error_text(&error));
    let config = AppConfig::read_or_create(&paths);
    let store = Store::open(paths.state_db());
    let git_timeout = config
        .as_ref()
        .map_or(default_git_timeout, |config| config.git_timeout_ms);
    let git = GitClient::new(Duration::from_millis(git_timeout));
    let active_profile = store
        .as_ref()
        .ok()
        .and_then(|store| store.active_profile().ok().flatten());
    let agent_health = agents.detect_all(
        active_profile
            .as_ref()
            .map(|profile| (profile.agent_id.as_str(), profile.agent_home.as_path())),
    );
    let state = LoadedState {
        paths: Some(&paths),
        directories_error,
        config: match &config {
            Ok(config) => Loaded::Ready(config),
            Err(error) => Loaded::Failed(safe_error_text(error)),
        },
        store: match &store {
            Ok(store) => Loaded::Ready(store),
            Err(error) => Loaded::Failed(safe_error_text(error)),
        },
    };
    build_report(&state, &git, &agents, verbose, &agent_health)
}

fn build_report(
    state: &LoadedState<'_>,
    git: &GitClient,
    agents: &AgentRegistry,
    verbose: bool,
    agent_health: &[crate::error::Result<AgentHealth>],
) -> DoctorReport {
    let mut checks = vec![DoctorCheck {
        name: "Application".into(),
        status: CheckStatus::Pass,
        summary: format!("{PRODUCT_NAME} {VERSION}"),
        action: None,
    }];

    if let Some(error) = &state.directories_error {
        checks.push(DoctorCheck {
            name: "State directories".into(),
            status: CheckStatus::Fail,
            summary: error.clone(),
            action: Some(
                "Make the ContextWake state directories writable real directories (not symlinks), or set CONTEXTWAKE_HOME to a writable directory."
                    .into(),
            ),
        });
    }

    checks.push(configuration_check(state));
    checks.push(state_database_check(state));

    checks.push(match git.version() {
        Ok(version) => DoctorCheck {
            name: "Git".into(),
            status: CheckStatus::Pass,
            summary: version,
            action: None,
        },
        Err(error) => DoctorCheck {
            name: "Git".into(),
            status: CheckStatus::Warning,
            summary: sanitize_terminal(&error.to_string()),
            action: Some(
                "Install Git or add it to PATH for branch, change and diff tracking. Without Git, workspaces are handled as ordinary directories."
                    .into(),
            ),
        },
    });

    for (adapter, agent_health) in agents.implemented().zip(agent_health) {
        let display_name = adapter.display_name();
        checks.push(match agent_health {
            Ok(health) if health.installed => DoctorCheck {
                name: display_name.into(),
                status: CheckStatus::Pass,
                summary: if verbose {
                    format!(
                        "{} · {} ({})",
                        health.version.as_deref().unwrap_or("version unavailable"),
                        health.auth_state.as_str(),
                        health.executable.as_ref().map_or_else(
                            || "path unavailable".into(),
                            |path| { sanitize_terminal(&path.display().to_string()) }
                        )
                    )
                } else {
                    format!(
                        "{} · {}",
                        health.version.as_deref().unwrap_or("version unavailable"),
                        health.auth_state.as_str()
                    )
                },
                action: None,
            },
            Ok(health) => DoctorCheck {
                name: display_name.into(),
                status: CheckStatus::Warning,
                summary: if health.executable.is_none() {
                    "Not installed".into()
                } else {
                    health.message.clone()
                },
                action: Some(format!(
                    "Install {display_name} or configure its CONTEXTWAKE_*_BIN override."
                )),
            },
            Err(error) => DoctorCheck {
                name: display_name.into(),
                status: CheckStatus::Warning,
                summary: sanitize_terminal(&error.to_string()),
                action: Some("Run the agent's own doctor command for deeper diagnostics.".into()),
            },
        });
    }

    if let Loaded::Ready(store) = state.store {
        checks.push(active_workspace_check(store));
        checks.extend(profile_homes_check(store, agents));
    }

    checks.push(match state.store {
        Loaded::Ready(store) => authentication_check(store, agents, agent_health),
        Loaded::Failed(_) | Loaded::NotChecked(_) => DoctorCheck {
            name: "Authentication".into(),
            status: CheckStatus::Warning,
            summary: "not checked: the state database is unavailable".into(),
            action: None,
        },
    });

    if let Some(warning) = state.paths.and_then(AppPaths::permission_warning) {
        checks.push(DoctorCheck {
            name: "State permissions".into(),
            status: CheckStatus::Warning,
            summary: sanitize_terminal(&warning),
            action: Some(
                "Move CONTEXTWAKE_HOME inside your user profile, or remove inherited access to that folder (see docs/configuration.md).".into(),
            ),
        });
    }
    let reserved_settings = match &state.config {
        Loaded::Ready(config) => config.reserved_setting_notices(),
        Loaded::Failed(_) | Loaded::NotChecked(_) => Vec::new(),
    };
    if !reserved_settings.is_empty() {
        checks.push(DoctorCheck {
            name: "Reserved settings".into(),
            status: CheckStatus::Warning,
            summary: sanitize_terminal(&reserved_settings.join("; ")),
            action: Some("Remove these settings from config.toml; they have no effect.".into()),
        });
    }

    checks.push(DoctorCheck {
        name: "Credential boundary".into(),
        status: CheckStatus::Pass,
        summary: "ContextWake stores profile metadata only; coding agents own credentials, and configuration/credential isolation is reported per adapter rather than assumed"
            .into(),
        action: None,
    });

    checks.push(DoctorCheck {
        name: "Terminal".into(),
        status: if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warning
        },
        summary: if std::io::stdout().is_terminal() {
            "interactive terminal detected".into()
        } else {
            "non-interactive output; use CLI commands or run 'ctx' in a terminal".into()
        },
        action: None,
    });

    DoctorReport {
        application: PRODUCT_NAME.into(),
        version: VERSION.into(),
        checks,
    }
}

fn configuration_check(state: &LoadedState<'_>) -> DoctorCheck {
    let config_file = state.paths.map_or_else(
        || "config.toml".into(),
        |paths| sanitize_terminal(&paths.config_file().display().to_string()),
    );
    let failed = |summary: String| DoctorCheck {
        name: "Configuration".into(),
        status: CheckStatus::Fail,
        summary,
        action: Some(format!(
            "Correct or remove the reported setting in {config_file}. 'ctx config set' can repair out-of-range values, and 'ctx config validate' checks the file again."
        )),
    };
    match &state.config {
        Loaded::Ready(config) => match config.validate() {
            Ok(()) => DoctorCheck {
                name: "Configuration".into(),
                status: CheckStatus::Pass,
                summary: config_file.clone(),
                action: None,
            },
            Err(error) => failed(safe_error_text(&error)),
        },
        Loaded::Failed(summary) => failed(summary.clone()),
        Loaded::NotChecked(reason) => not_checked("Configuration", reason),
    }
}

fn state_database_check(state: &LoadedState<'_>) -> DoctorCheck {
    match &state.store {
        Loaded::Ready(store) => match store.integrity_check() {
            Ok(value) if value == "ok" => DoctorCheck {
                name: "State database".into(),
                status: CheckStatus::Pass,
                summary: "SQLite quick_check: ok".into(),
                action: None,
            },
            Ok(value) => DoctorCheck {
                name: "State database".into(),
                status: CheckStatus::Fail,
                summary: sanitize_terminal(&value),
                action: Some("Back up the ContextWake data directory before recovery.".into()),
            },
            Err(error) => DoctorCheck {
                name: "State database".into(),
                status: CheckStatus::Fail,
                summary: sanitize_terminal(&error.to_string()),
                action: Some("Check data-directory permissions and available disk space.".into()),
            },
        },
        Loaded::Failed(summary) => DoctorCheck {
            name: "State database".into(),
            status: CheckStatus::Fail,
            summary: summary.clone(),
            action: Some(format!(
                "If a newer ContextWake build upgraded this database, use that build. Otherwise back up {} before recovery.",
                state.paths.map_or_else(
                    || "the ContextWake data directory".into(),
                    |paths| sanitize_terminal(&paths.data_dir.display().to_string())
                )
            )),
        },
        Loaded::NotChecked(reason) => not_checked("State database", reason),
    }
}

fn active_workspace_check(store: &Store) -> DoctorCheck {
    let name = "Active workspace";
    match store.active_workspace() {
        Ok(None) => DoctorCheck {
            name: name.into(),
            status: CheckStatus::Pass,
            summary: "none selected; commands use the workspace of the current directory".into(),
            action: None,
        },
        Ok(Some(workspace)) if workspace.path.is_dir() => DoctorCheck {
            name: name.into(),
            status: CheckStatus::Pass,
            summary: format!(
                "{} ({})",
                sanitize_terminal(&workspace.display_name),
                sanitize_terminal(&workspace.path.display().to_string())
            ),
            action: None,
        },
        Ok(Some(workspace)) => DoctorCheck {
            name: name.into(),
            status: CheckStatus::Warning,
            summary: format!(
                "{}: directory {} is missing or unreachable",
                sanitize_terminal(&workspace.display_name),
                sanitize_terminal(&workspace.path.display().to_string())
            ),
            action: Some(format!(
                "Open another workspace with 'ctx workspace open <workspace>', or remove this one with 'ctx workspace remove {} --yes'.",
                workspace.id
            )),
        },
        Err(error) => DoctorCheck {
            name: name.into(),
            status: CheckStatus::Warning,
            summary: safe_error_text(&error),
            action: Some("Select a workspace with 'ctx workspace open <workspace>'.".into()),
        },
    }
}

fn authentication_check(
    store: &Store,
    agents: &AgentRegistry,
    agent_health: &[crate::error::Result<AgentHealth>],
) -> DoctorCheck {
    match store.active_profile().ok().flatten() {
        Some(profile) => {
            let auth = agents
                .implemented()
                .zip(agent_health)
                .find(|(adapter, _)| adapter.id() == profile.agent_id)
                .and_then(|(_, health)| health.as_ref().ok())
                .map_or(AuthState::Unknown, |health| health.auth_state);
            let agent = agents
                .get(&profile.agent_id)
                .map_or(profile.agent_id.as_str(), |adapter| adapter.display_name());
            DoctorCheck {
                name: "Authentication".into(),
                status: if auth == AuthState::SignedIn {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warning
                },
                summary: format!(
                    "{}: {} ({})",
                    sanitize_terminal(&profile.display_name),
                    auth.as_str(),
                    home_mode_text(profile.home_mode, agent)
                ),
                action: (auth != AuthState::SignedIn).then(|| match profile.home_mode {
                    ProfileHomeMode::AgentDefault => format!(
                        "Sign in to {agent} the way you normally do, or run 'ctx profile login {}' (this signs in your normal {agent} login).",
                        profile.name
                    ),
                    ProfileHomeMode::Isolated => {
                        format!("Run 'ctx profile login {}'.", profile.name)
                    }
                }),
            }
        }
        None => DoctorCheck {
            name: "Authentication".into(),
            status: CheckStatus::Warning,
            summary: "no active ContextWake profile".into(),
            action: Some("Run 'ctx profile add Personal'.".into()),
        },
    }
}

fn home_mode_text(mode: ProfileHomeMode, agent: &str) -> String {
    match mode {
        ProfileHomeMode::AgentDefault => format!("existing {agent} sign-in"),
        ProfileHomeMode::Isolated => "separate ContextWake sign-in".into(),
    }
}

/// Lists every profile with the sign-in it uses, and warns when a profile that
/// uses an agent's existing sign-in no longer matches the home the agent uses
/// now (for example after `CLAUDE_CONFIG_DIR` or `CODEX_HOME` changed). Nothing
/// is probed or written.
fn profile_homes_check(store: &Store, agents: &AgentRegistry) -> Option<DoctorCheck> {
    let profiles = match store.list_profiles() {
        Ok(profiles) if profiles.is_empty() => return None,
        Ok(profiles) => profiles,
        Err(error) => {
            return Some(DoctorCheck {
                name: "Profiles".into(),
                status: CheckStatus::Warning,
                summary: safe_error_text(&error),
                action: Some("Run 'ctx profile list' for details.".into()),
            });
        }
    };
    let mut entries = Vec::new();
    let mut mismatches = Vec::new();
    for profile in &profiles {
        let adapter = agents.get(&profile.agent_id).ok();
        let agent = adapter.map_or(profile.agent_id.as_str(), |adapter| adapter.display_name());
        entries.push(format!(
            "{} ({})",
            sanitize_terminal(&profile.display_name),
            home_mode_text(profile.home_mode, agent)
        ));
        if profile.home_mode != ProfileHomeMode::AgentDefault {
            continue;
        }
        let current = adapter.and_then(AgentAdapter::default_home);
        if !current
            .as_deref()
            .is_some_and(|current| paths_equivalent(current, &profile.agent_home))
        {
            mismatches.push(format!(
                "{} records {} but {agent} now uses {}",
                sanitize_terminal(&profile.display_name),
                sanitize_terminal(&profile.agent_home.display().to_string()),
                current.map_or_else(
                    || "no default home".into(),
                    |current| sanitize_terminal(&current.display().to_string())
                )
            ));
        }
    }
    Some(if mismatches.is_empty() {
        DoctorCheck {
            name: "Profiles".into(),
            status: CheckStatus::Pass,
            summary: entries.join(", "),
            action: None,
        }
    } else {
        DoctorCheck {
            name: "Profiles".into(),
            status: CheckStatus::Warning,
            summary: mismatches.join("; "),
            action: Some(
                "Restore the agent's home variable that was in effect when the profile was created, or remove the profile ('ctx profile remove <name> --yes'; the agent's home is not changed) and add it again."
                    .into(),
            ),
        }
    })
}

fn not_checked(name: &str, reason: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status: CheckStatus::Warning,
        summary: format!("not checked: {reason}"),
        action: None,
    }
}

fn safe_error_text(error: &ContextWakeError) -> String {
    SecretScanner::new()
        .redact(&sanitize_terminal(&error.to_string()))
        .text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<'a>(report: &'a DoctorReport, name: &str) -> &'a DoctorCheck {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("missing {name} check"))
    }

    #[test]
    fn unloadable_configuration_and_database_become_failed_checks() {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path());
        let state = LoadedState {
            paths: Some(&paths),
            directories_error: None,
            config: Loaded::Failed(
                "configuration error: git_timeout_ms must be between 100 and 30000".into(),
            ),
            store: Loaded::Failed(
                "invalid data: state schema 99 is not supported by this build".into(),
            ),
        };
        let agents = AgentRegistry::discover();
        let results = agents
            .implemented()
            .map(|adapter| {
                Ok(AgentHealth {
                    agent_id: adapter.id().into(),
                    installed: false,
                    executable: None,
                    version: None,
                    auth_state: AuthState::Unknown,
                    message: "not installed".into(),
                })
            })
            .collect::<Vec<_>>();
        let git =
            GitClient::with_executable("contextwake-test-missing-git", Duration::from_millis(100));
        let report = build_report(&state, &git, &agents, false, &results);

        assert!(report.has_failures());
        let configuration = check(&report, "Configuration");
        assert_eq!(configuration.status, CheckStatus::Fail);
        assert!(configuration.summary.contains("git_timeout_ms"));
        assert!(
            configuration
                .action
                .as_deref()
                .is_some_and(|action| action.contains("ctx config set"))
        );
        let database = check(&report, "State database");
        assert_eq!(database.status, CheckStatus::Fail);
        assert!(database.summary.contains("schema 99"));
        assert_eq!(
            check(&report, "Authentication").status,
            CheckStatus::Warning
        );
        assert!(
            !report
                .checks
                .iter()
                .any(|check| check.name == "Active workspace"),
            "no workspace check without a database"
        );
        let git_check = check(&report, "Git");
        assert_eq!(git_check.status, CheckStatus::Warning);
        assert!(
            git_check
                .action
                .as_deref()
                .is_some_and(|action| action.contains("ordinary directories"))
        );
    }

    #[test]
    fn profile_sign_in_modes_are_reported_and_moved_default_homes_warn() {
        use crate::app::profile_tests::{ExistingLoginAgent, application_with, user_home};
        use crate::app::{NewProfile, ProfileHomeChoice};

        let root = tempfile::tempdir().expect("root");
        let home = user_home(root.path());
        let agent = ExistingLoginAgent::new(root.path(), Some(home.clone()));
        let app = application_with(root.path(), agent.clone());
        assert!(profile_homes_check(&app.store, &app.agents).is_none());
        for (name, choice) in [
            ("Mine", ProfileHomeChoice::ExistingLogin),
            ("Separate", ProfileHomeChoice::Isolated),
        ] {
            app.add_profile(&NewProfile {
                name,
                display_name: None,
                description: None,
                agent_id: "claude",
                model_provider_id: None,
                model_preference: None,
                allow_shared_identity: false,
                home: choice,
            })
            .expect("profile");
        }

        let check = profile_homes_check(&app.store, &app.agents).expect("profiles check");
        assert_eq!(check.status, CheckStatus::Pass);
        assert!(
            check
                .summary
                .contains("Mine (existing Claude Code sign-in)"),
            "{}",
            check.summary
        );
        assert!(
            check
                .summary
                .contains("Separate (separate ContextWake sign-in)"),
            "{}",
            check.summary
        );
        // The active profile (the first one created) reports its mode too.
        let results = app
            .agents
            .implemented()
            .map(|adapter| {
                Ok(AgentHealth {
                    agent_id: adapter.id().into(),
                    installed: true,
                    executable: None,
                    version: None,
                    auth_state: AuthState::SignedOut,
                    message: String::new(),
                })
            })
            .collect::<Vec<_>>();
        let authentication = authentication_check(&app.store, &app.agents, &results);
        assert!(
            authentication
                .summary
                .contains("existing Claude Code sign-in"),
            "{}",
            authentication.summary
        );
        assert!(
            authentication
                .action
                .as_deref()
                .is_some_and(|action| action.contains("normal Claude Code login")),
            "{:?}",
            authentication.action
        );

        // The agent's home variable changed since the profile was created.
        agent.set_default_home(Some(root.path().join("Other Home").join(".claude")));
        let check = profile_homes_check(&app.store, &app.agents).expect("profiles check");
        assert_eq!(check.status, CheckStatus::Warning);
        assert!(check.summary.contains("Other Home"), "{}", check.summary);
        assert!(
            check
                .action
                .as_deref()
                .is_some_and(|action| action.contains("ctx profile remove")),
            "{:?}",
            check.action
        );
    }

    #[test]
    fn missing_active_workspace_directory_is_reported_with_recovery_commands() {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path().join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        let directory = root.path().join("deleted-project");
        std::fs::create_dir(&directory).expect("project");
        let now = chrono::Utc::now();
        let workspace = crate::model::Workspace {
            id: uuid::Uuid::new_v4(),
            path: directory.clone(),
            display_name: "deleted-project".into(),
            trust_state: crate::model::TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        };
        store.upsert_workspace(&workspace).expect("workspace");
        store
            .set_active_workspace(workspace.id)
            .expect("active workspace");
        assert_eq!(
            active_workspace_check(&store).status,
            CheckStatus::Pass,
            "an existing active workspace passes"
        );

        std::fs::remove_dir(&directory).expect("delete project");
        let missing = active_workspace_check(&store);
        assert_eq!(missing.status, CheckStatus::Warning);
        assert!(missing.summary.contains("missing"));
        let action = missing.action.expect("recovery action");
        assert!(action.contains("ctx workspace open"));
        assert!(action.contains(&format!("ctx workspace remove {} --yes", workspace.id)));
    }
}
