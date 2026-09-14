#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use serde_json::json;

pub mod app;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod continuity;
pub mod doctor;
pub mod error;
pub mod git;
pub mod guardian;
pub mod handoff;
pub mod model;
pub mod paths;
pub mod provider;
pub mod security;
pub mod store;
pub mod tui;
pub mod validation;
pub mod workspace;

pub const PRODUCT_NAME: &str = "ContextWake";
pub const BINARY_NAME: &str = "ctx";
pub const LEGACY_BINARY_NAME: &str = "ctxwake";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the `ContextWake` CLI from either the canonical or compatibility binary.
pub fn run() -> ExitCode {
    let invoked_name = invoked_binary_name();
    let mut command = cli::Cli::command().name(BINARY_NAME).bin_name(BINARY_NAME);
    if invoked_name == LEGACY_BINARY_NAME {
        command = command
            .name(LEGACY_BINARY_NAME)
            .bin_name(LEGACY_BINARY_NAME);
    }
    let matches = command.get_matches();
    let cli = match cli::Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    let json_output = cli.json;
    match app::Application::discover().and_then(|application| application.run(cli)) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let safe = security::SecretScanner::new()
                .redact(&security::sanitize_terminal(&error.to_string()));
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
                eprintln!("\nRun 'ctx doctor' for privacy-safe diagnostics.");
            }
            ExitCode::FAILURE
        }
    }
}

fn invoked_binary_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|path| std::path::Path::new(&path).file_stem().map(OsString::from))
        .map_or_else(
            || BINARY_NAME.into(),
            |name| name.to_string_lossy().into_owned(),
        )
}

fn error_code(error: &error::ContextWakeError) -> &'static str {
    use error::ContextWakeError;
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
