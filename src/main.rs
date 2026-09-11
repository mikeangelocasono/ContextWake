use std::process::ExitCode;

use agentdeck::app::Application;
use agentdeck::cli::Cli;
use agentdeck::security::{SecretScanner, sanitize_terminal};
use clap::Parser;
use serde_json::json;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json;
    match Application::discover().and_then(|application| application.run(cli)) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let safe = SecretScanner::new().redact(&sanitize_terminal(&error.to_string()));
            if json_output {
                let payload = json!({
                    "code": error_code(&error),
                    "message": safe.text,
                    "retryable": false
                });
                if let Ok(encoded) = serde_json::to_string_pretty(&payload) {
                    eprintln!("{encoded}");
                }
            } else {
                eprintln!("AgentDeck could not complete the request.\n\n{}", safe.text);
                eprintln!("\nRun 'adeck doctor' for privacy-safe diagnostics.");
            }
            ExitCode::FAILURE
        }
    }
}

fn error_code(error: &agentdeck::error::AgentDeckError) -> &'static str {
    use agentdeck::error::AgentDeckError;
    match error {
        AgentDeckError::CapabilityUnavailable(_) => "ADK-AGENT-CAPABILITY-UNAVAILABLE",
        AgentDeckError::ProfileNotFound(_) => "ADK-PROFILE-NOT-FOUND",
        AgentDeckError::WorkspaceNotFound(_) => "ADK-WORKSPACE-NOT-FOUND",
        AgentDeckError::SessionNotFound(_) => "ADK-SESSION-NOT-FOUND",
        AgentDeckError::CheckpointNotFound(_) => "ADK-CHECKPOINT-NOT-FOUND",
        AgentDeckError::HandoffNotFound(_) => "ADK-HANDOFF-NOT-FOUND",
        AgentDeckError::UnsafePath(_) | AgentDeckError::UntrustedConfiguration(_) => {
            "ADK-SECURITY-BOUNDARY"
        }
        AgentDeckError::Provider(_) => "ADK-AGENT-ERROR",
        AgentDeckError::Git(_) => "ADK-GIT-ERROR",
        AgentDeckError::Cancelled => "ADK-CANCELLED",
        _ => "ADK-ERROR",
    }
}
