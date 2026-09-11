use std::process::ExitCode;

use clap::Parser;
use contextwake::app::Application;
use contextwake::cli::Cli;
use contextwake::security::{SecretScanner, sanitize_terminal};
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
                eprintln!(
                    "ContextWake could not complete the request.\n\n{}",
                    safe.text
                );
                eprintln!("\nRun 'ctxwake doctor' for privacy-safe diagnostics.");
            }
            ExitCode::FAILURE
        }
    }
}

fn error_code(error: &contextwake::error::ContextWakeError) -> &'static str {
    use contextwake::error::ContextWakeError;
    match error {
        ContextWakeError::CapabilityUnavailable(_) => "CWK-AGENT-CAPABILITY-UNAVAILABLE",
        ContextWakeError::ProfileNotFound(_) => "CWK-PROFILE-NOT-FOUND",
        ContextWakeError::WorkspaceNotFound(_) => "CWK-WORKSPACE-NOT-FOUND",
        ContextWakeError::SessionNotFound(_) => "CWK-SESSION-NOT-FOUND",
        ContextWakeError::CheckpointNotFound(_) => "CWK-CHECKPOINT-NOT-FOUND",
        ContextWakeError::HandoffNotFound(_) => "CWK-HANDOFF-NOT-FOUND",
        ContextWakeError::UnsafePath(_) | ContextWakeError::UntrustedConfiguration(_) => {
            "CWK-SECURITY-BOUNDARY"
        }
        ContextWakeError::Provider(_) => "CWK-AGENT-ERROR",
        ContextWakeError::Git(_) => "CWK-GIT-ERROR",
        ContextWakeError::Cancelled => "CWK-CANCELLED",
        _ => "CWK-ERROR",
    }
}
