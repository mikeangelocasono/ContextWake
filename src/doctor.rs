use std::io::IsTerminal;

use serde::Serialize;

use crate::config::AppConfig;
use crate::git::GitClient;
use crate::model::AuthState;
use crate::paths::AppPaths;
use crate::provider::AgentRegistry;
use crate::security::sanitize_terminal;
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

pub fn run_doctor(
    paths: &AppPaths,
    config: &AppConfig,
    store: &Store,
    git: &GitClient,
    agents: &AgentRegistry,
    verbose: bool,
) -> DoctorReport {
    let mut checks = vec![DoctorCheck {
        name: "Application".into(),
        status: CheckStatus::Pass,
        summary: format!("{PRODUCT_NAME} {VERSION}"),
        action: None,
    }];

    checks.push(match config.validate() {
        Ok(()) => DoctorCheck {
            name: "Configuration".into(),
            status: CheckStatus::Pass,
            summary: sanitize_terminal(&paths.config_file().display().to_string()),
            action: None,
        },
        Err(error) => DoctorCheck {
            name: "Configuration".into(),
            status: CheckStatus::Fail,
            summary: sanitize_terminal(&error.to_string()),
            action: Some("Run 'adeck config validate' after correcting config.toml.".into()),
        },
    });

    checks.push(match store.integrity_check() {
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
            action: Some("Back up the AgentDeck data directory before recovery.".into()),
        },
        Err(error) => DoctorCheck {
            name: "State database".into(),
            status: CheckStatus::Fail,
            summary: sanitize_terminal(&error.to_string()),
            action: Some("Check data-directory permissions and available disk space.".into()),
        },
    });

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
            action: Some("Install Git; non-Git workspaces remain supported.".into()),
        },
    });

    let active_profile = store.active_profile().ok().flatten();
    for adapter in agents.implemented() {
        let home = active_profile
            .as_ref()
            .filter(|profile| profile.agent_id == adapter.id())
            .map(|profile| profile.agent_home.as_path());
        let agent_health = adapter.detect(home);
        checks.push(match agent_health {
            Ok(health) if health.installed => DoctorCheck {
                name: adapter.display_name().into(),
                status: CheckStatus::Pass,
                summary: if verbose {
                    format!(
                        "{} ({})",
                        health.version.as_deref().unwrap_or("version unavailable"),
                        health.executable.as_ref().map_or_else(
                            || "path unavailable".into(),
                            |path| { sanitize_terminal(&path.display().to_string()) }
                        )
                    )
                } else {
                    health
                        .version
                        .unwrap_or_else(|| "version unavailable".into())
                },
                action: None,
            },
            Ok(health) => DoctorCheck {
                name: adapter.display_name().into(),
                status: CheckStatus::Warning,
                summary: health.message,
                action: Some(format!(
                    "Install {} or configure its AGENTDECK_*_BIN override.",
                    adapter.display_name()
                )),
            },
            Err(error) => DoctorCheck {
                name: adapter.display_name().into(),
                status: CheckStatus::Warning,
                summary: sanitize_terminal(&error.to_string()),
                action: Some("Run the agent's own doctor command for deeper diagnostics.".into()),
            },
        });
    }

    checks.push(match active_profile {
        Some(profile) => {
            let auth = agents
                .get(&profile.agent_id)
                .and_then(|adapter| adapter.auth_status(&profile.agent_home))
                .unwrap_or(AuthState::Unknown);
            DoctorCheck {
                name: "Authentication".into(),
                status: if auth == AuthState::SignedIn {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warning
                },
                summary: format!("{}: {}", profile.display_name, auth.as_str()),
                action: (auth != AuthState::SignedIn)
                    .then(|| format!("Run 'adeck profile login {}'.", profile.name)),
            }
        }
        None => DoctorCheck {
            name: "Authentication".into(),
            status: CheckStatus::Warning,
            summary: "no active AgentDeck profile".into(),
            action: Some("Run 'adeck profile add Personal'.".into()),
        },
    });

    checks.push(DoctorCheck {
        name: "Credential boundary".into(),
        status: CheckStatus::Pass,
        summary: "AgentDeck stores profile metadata only; each coding agent owns credentials in an isolated configuration root"
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
            "non-interactive output; use CLI commands or run 'adeck' in a terminal".into()
        },
        action: None,
    });

    DoctorReport {
        application: PRODUCT_NAME.into(),
        version: VERSION.into(),
        checks,
    }
}
