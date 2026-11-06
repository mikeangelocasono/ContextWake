use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, Result};
use crate::model::{TrustState, ValidationResult, ValidationStatus, Workspace};
use crate::paths::canonicalize_simplified;
use crate::security::{SecretScanner, sanitize_terminal};
use crate::workspace::CommandSpec;

/// Upper bound for reaping a command after it was killed. Standard streams
/// are null, so no grandchild can hold a pipe open; the bound only guards
/// against a process that ignores termination.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

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
        // Pass the conventional form (no `\\?\` prefix), like every other path
        // handed to an external program. cmd.exe, which runs .cmd/.bat commands
        // such as npm.cmd, cannot use a verbatim directory and would fall back to
        // C:\Windows; the standard library currently strips the prefix itself,
        // and this keeps the behavior independent of that detail.
        let working_directory =
            canonicalize_simplified(&workspace.path).map_err(|source| ContextWakeError::Io {
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
        let status = match child.wait_timeout(self.timeout) {
            Ok(status) => status,
            Err(error) => {
                terminate(&mut child);
                return Err(ContextWakeError::InvalidData(format!(
                    "could not wait for validation command: {error}"
                )));
            }
        };
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
                terminate(&mut child);
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

/// Stops a validation command and everything it started, then reaps it with a
/// bounded wait. `Child::kill` only ends the direct child, which would leave
/// e.g. the node process behind `npm.cmd` or the test binaries of `cargo test`
/// running after the timeout was recorded.
fn terminate(child: &mut Child) {
    #[cfg(windows)]
    kill_process_tree(child.id());
    let _ = child.kill();
    let _ = child.wait_timeout(REAP_TIMEOUT);
}

/// Windows has no process groups for plain children; `taskkill /T` walks the
/// parent/child tree from the still-running direct child.
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let system_root = std::env::var_os("SystemRoot")
        .map_or_else(|| "C:\\Windows".into(), std::path::PathBuf::from);
    let taskkill = system_root.join("System32").join("taskkill.exe");
    let Ok(mut killer) = Command::new(taskkill)
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if !matches!(killer.wait_timeout(REAP_TIMEOUT), Ok(Some(_))) {
        let _ = killer.kill();
        let _ = killer.wait_timeout(REAP_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

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

    #[cfg(windows)]
    fn system32(program: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        format!("{root}\\System32\\{program}")
    }

    #[cfg(windows)]
    fn sleeping_command() -> CommandSpec {
        CommandSpec {
            executable: system32("PING.EXE"),
            args: vec!["-n".into(), "30".into(), "127.0.0.1".into()],
        }
    }

    #[cfg(unix)]
    fn sleeping_command() -> CommandSpec {
        CommandSpec {
            executable: "sleep".into(),
            args: vec!["30".into()],
        }
    }

    #[test]
    fn timed_out_command_is_killed_and_reported() {
        let root = tempfile::tempdir().expect("tempdir");
        let runner = ValidationRunner::new(Duration::from_millis(200));
        let started = Instant::now();
        let results = runner
            .run(
                &workspace(root.path(), TrustState::Trusted),
                &[sleeping_command()],
            )
            .expect("result");
        assert_eq!(results[0].status, ValidationStatus::Failed);
        assert_eq!(results[0].summary, "timed out after 200 ms");
        // The 30-second command was killed, not merely abandoned: without the
        // kill, the bounded reap alone would take the full REAP_TIMEOUT.
        assert!(started.elapsed() < REAP_TIMEOUT, "{:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_stops_the_direct_child_on_unix() {
        let root = tempfile::tempdir().expect("tempdir");
        // The shell writes the marker only if it survives the timeout.
        let runner = ValidationRunner::new(Duration::from_millis(300));
        let results = runner
            .run(
                &workspace(root.path(), TrustState::Trusted),
                &[CommandSpec {
                    executable: "sh".into(),
                    args: vec!["-c".into(), "sleep 2; touch child-marker.txt".into()],
                }],
            )
            .expect("result");
        assert_eq!(results[0].summary, "timed out after 300 ms");
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !root.path().join("child-marker.txt").exists(),
            "the timed-out command kept running"
        );
    }

    #[cfg(windows)]
    #[test]
    fn batch_commands_run_in_the_workspace_not_the_windows_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("workspace-marker.txt"), "here").expect("marker");
        let script = root.path().join("check-cwd.cmd");
        std::fs::write(
            &script,
            "@if exist workspace-marker.txt (exit /b 0) else (exit /b 3)\r\n",
        )
        .expect("script");
        let runner = ValidationRunner::new(Duration::from_secs(30));
        let results = runner
            .run(
                &workspace(root.path(), TrustState::Trusted),
                &[CommandSpec {
                    executable: script.to_string_lossy().into_owned(),
                    args: Vec::new(),
                }],
            )
            .expect("result");
        assert_eq!(
            (results[0].status, results[0].summary.as_str()),
            (ValidationStatus::Passed, "exit status 0")
        );
    }

    #[cfg(windows)]
    #[test]
    fn timeout_kills_the_whole_process_tree() {
        let root = tempfile::tempdir().expect("tempdir");
        // The direct child (cmd.exe running the script) starts a detached
        // grandchild that writes a marker after about two seconds, then keeps
        // the direct child busy well past the timeout.
        let script = root.path().join("tree.cmd");
        std::fs::write(
            &script,
            "@echo off\r\nstart \"\" /b cmd /d /c \"ping -n 3 127.0.0.1 >nul & echo late> grandchild-marker.txt\"\r\nping -n 30 127.0.0.1 >nul\r\n",
        )
        .expect("script");
        let runner = ValidationRunner::new(Duration::from_millis(700));
        let results = runner
            .run(
                &workspace(root.path(), TrustState::Trusted),
                &[CommandSpec {
                    executable: script.to_string_lossy().into_owned(),
                    args: Vec::new(),
                }],
            )
            .expect("result");
        assert_eq!(results[0].summary, "timed out after 700 ms");
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !root.path().join("grandchild-marker.txt").exists(),
            "a grandchild of the timed-out command kept running"
        );
    }
}
