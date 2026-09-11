use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, Result};
use crate::model::{TrustState, ValidationResult, ValidationStatus, Workspace};
use crate::security::{SecretScanner, sanitize_terminal};
use crate::workspace::CommandSpec;

#[derive(Clone, Debug)]
pub struct ValidationRunner {
    timeout: Duration,
}

impl ValidationRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn run(
        &self,
        workspace: &Workspace,
        commands: &[CommandSpec],
    ) -> Result<Vec<ValidationResult>> {
        if workspace.trust_state != TrustState::Trusted {
            return Err(ContextWakeError::UntrustedConfiguration(
                "validation requires a trusted workspace and an explicit validate action".into(),
            ));
        }
        if commands.is_empty() {
            return Err(ContextWakeError::Configuration(
                "no validation commands are declared in .contextwake/project.toml".into(),
            ));
        }
        let working_directory =
            workspace
                .path
                .canonicalize()
                .map_err(|source| ContextWakeError::Io {
                    path: workspace.path.clone(),
                    source,
                })?;
        commands
            .iter()
            .map(|command| self.run_one(&working_directory, command))
            .collect()
    }

    fn run_one(
        &self,
        working_directory: &std::path::Path,
        specification: &CommandSpec,
    ) -> Result<ValidationResult> {
        let scanner = SecretScanner::new();
        let safe_command = std::iter::once(&specification.executable)
            .chain(specification.args.iter())
            .map(|value| scanner.redact(&sanitize_terminal(value)).text)
            .collect();
        let observed_at = Utc::now();
        let child = Command::new(&specification.executable)
            .args(&specification.args)
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                return Ok(ValidationResult {
                    command: safe_command,
                    status: ValidationStatus::Failed,
                    observed_at,
                    summary: format!("could not start command ({:?})", error.kind()),
                });
            }
        };
        let status = child.wait_timeout(self.timeout).map_err(|error| {
            ContextWakeError::InvalidData(format!("could not wait for validation command: {error}"))
        })?;
        let (status, summary) = match status {
            Some(status) if status.success() => (ValidationStatus::Passed, "exit status 0".into()),
            Some(status) => (
                ValidationStatus::Failed,
                status.code().map_or_else(
                    || "terminated without an exit code".into(),
                    |code| format!("exit status {code}"),
                ),
            ),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                (
                    ValidationStatus::Failed,
                    format!("timed out after {} ms", self.timeout.as_millis()),
                )
            }
        };
        Ok(ValidationResult {
            command: safe_command,
            status,
            observed_at,
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn workspace(root: &std::path::Path, trust_state: TrustState) -> Workspace {
        let now = Utc::now();
        Workspace {
            id: Uuid::new_v4(),
            path: root.to_path_buf(),
            display_name: "validation fixture".into(),
            trust_state,
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

    #[test]
    fn refuses_untrusted_workspace_commands() {
        let root = tempfile::tempdir().expect("tempdir");
        let runner = ValidationRunner::new(Duration::from_secs(1));
        let result = runner.run(
            &workspace(root.path(), TrustState::Untrusted),
            &[CommandSpec {
                executable: "git".into(),
                args: vec!["--version".into()],
            }],
        );
        assert!(matches!(
            result,
            Err(ContextWakeError::UntrustedConfiguration(_))
        ));
    }

    #[test]
    fn records_failure_without_persisting_secret_arguments() {
        let root = tempfile::tempdir().expect("tempdir");
        let runner = ValidationRunner::new(Duration::from_secs(1));
        let results = runner
            .run(
                &workspace(root.path(), TrustState::Trusted),
                &[CommandSpec {
                    executable: "contextwake-validation-command-that-does-not-exist".into(),
                    args: vec!["sk-abcdefghijklmnopqrstuvwxyz".into()],
                }],
            )
            .expect("result");
        assert_eq!(results[0].status, ValidationStatus::Failed);
        assert!(!results[0].command.join(" ").contains("sk-"));
    }
}
