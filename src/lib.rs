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
    let binary_name = invoked_binary_name();
    let command = cli::Cli::command().name(binary_name).bin_name(binary_name);
    let matches = command.get_matches();
    let cli = match cli::Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    let json_output = cli.json;
    match dispatch(cli, binary_name) {
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

/// Version and completion need no local state, so they work even when it is
/// broken. `doctor` and `config` fall back to a degraded mode when the state
/// cannot be initialized, because every startup error points to them.
fn dispatch(cli: cli::Cli, binary_name: &'static str) -> error::Result<i32> {
    match &cli.command {
        Some(cli::Command::Version) => return app::print_version(binary_name, cli.json),
        Some(cli::Command::Completion(args)) => {
            return app::print_completion(args.shell, binary_name);
        }
        _ => {}
    }
    let tolerates_startup_failure = matches!(
        cli.command,
        Some(cli::Command::Doctor(_) | cli::Command::Config(_))
    );
    match app::Application::discover() {
        Ok(application) => application.run(cli),
        Err(error) if tolerates_startup_failure => app::run_without_state(cli, error),
        Err(error) => Err(error),
    }
}

/// `ctxwake` when the process was started through the compatibility alias,
/// otherwise `ctx`.
fn invoked_binary_name() -> &'static str {
    let stem = std::env::args_os()
        .next()
        .and_then(|path| std::path::Path::new(&path).file_stem().map(OsString::from));
    match stem {
        Some(stem) if is_legacy_binary_name(&stem.to_string_lossy()) => LEGACY_BINARY_NAME,
        _ => BINARY_NAME,
    }
}

/// Windows file names are case-insensitive, so `CTXWAKE.EXE` is the alias too.
fn is_legacy_binary_name(stem: &str) -> bool {
    if cfg!(windows) {
        stem.eq_ignore_ascii_case(LEGACY_BINARY_NAME)
    } else {
        stem == LEGACY_BINARY_NAME
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_alias_is_recognized_by_file_stem() {
        assert!(is_legacy_binary_name("ctxwake"));
        assert!(!is_legacy_binary_name("ctx"));
        assert!(!is_legacy_binary_name("ctxwake-old"));
        // Windows file names are case-insensitive; Unix names are not.
        assert_eq!(is_legacy_binary_name("CTXWAKE"), cfg!(windows));
    }
}
