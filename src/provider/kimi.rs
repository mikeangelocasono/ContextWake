use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::{ContextWakeError, Result};
use crate::model::{
    AgentCapabilities, AgentHealth, AuthState, Capability, CapabilityMaturity, CapabilitySupport,
    DiscoveredAgentSession, ModelProvider,
};
use crate::provider::common::{
    contained_existing_path, ensure_profile_directory, read_file_bounded, run_probe,
    safe_agent_text, safe_probe_diagnostic,
};
use crate::provider::{AcpTransport, AgentAdapter};
use crate::security::validate_external_reference;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_INDEX_LIMIT: usize = 8 * 1024 * 1024;
const SESSION_STATE_LIMIT: usize = 131_072;
const PROVIDER_RESULT_LIMIT: usize = 500;

#[derive(Clone, Debug)]
pub struct KimiAdapter {
    executable: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KimiSessionIndex {
    session_id: String,
    session_dir: PathBuf,
    work_dir: PathBuf,
}

impl KimiAdapter {
    pub fn discover() -> Self {
        let executable =
            std::env::var_os("CONTEXTWAKE_KIMI_BIN").map_or_else(default_executable, PathBuf::from);
        Self { executable }
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    fn base_command(&self, agent_home: Option<&Path>) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(home) = agent_home {
            command.env("KIMI_CODE_HOME", home);
        }
        command
    }

    fn partial(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Partial,
            maturity: CapabilityMaturity::Heuristic,
            detail: detail.into(),
        }
    }

    fn unknown(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Unknown,
            maturity: CapabilityMaturity::Unavailable,
            detail: detail.into(),
        }
    }

    fn unavailable(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Unsupported,
            maturity: CapabilityMaturity::Unavailable,
            detail: detail.into(),
        }
    }
}

fn default_executable() -> PathBuf {
    #[cfg(windows)]
    {
        find_windows_executable("kimi.exe")
            .unwrap_or_else(|| PathBuf::from(r"C:\__contextwake_missing__\kimi.exe"))
    }
    #[cfg(not(windows))]
    {
        super::find_safe_on_path("kimi")
            .unwrap_or_else(|| PathBuf::from("/__contextwake_missing__/kimi"))
    }
}

#[cfg(windows)]
fn find_windows_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let current = std::env::current_dir().ok()?.canonicalize().ok()?;
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| super::safe_executable_candidate(candidate, &current))
}

fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    if stdout.trim().is_empty() {
        String::from_utf8_lossy(stderr).into_owned()
    } else {
        stdout.into_owned()
    }
}

fn has_kimi_signature(version: &str, help: &str) -> bool {
    let version = version.to_ascii_lowercase();
    let help = help.to_ascii_lowercase();
    !version.trim().is_empty()
        && help.contains("kimi")
        && help.contains("--session")
        && help.contains("--output-format")
        && help.contains("acp")
}

fn value_datetime(value: &serde_json::Value, key: &str) -> Option<DateTime<Utc>> {
    value
        .get(key)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn parse_session_state(
    bytes: &[u8],
) -> Result<(Option<String>, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        ContextWakeError::Provider(format!("Kimi session state is not valid JSON: {error}"))
    })?;
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(safe_agent_text)
        .filter(|title| !title.is_empty());
    let created_at =
        value_datetime(&value, "createdAt").or_else(|| value_datetime(&value, "created_at"));
    let updated_at =
        value_datetime(&value, "updatedAt").or_else(|| value_datetime(&value, "updated_at"));
    Ok((title, created_at, updated_at))
}

fn session_directory(agent_home: &Path, session_dir: &Path) -> Option<PathBuf> {
    let root = agent_home.join("sessions");
    let candidate = if session_dir.is_absolute() {
        session_dir.to_path_buf()
    } else {
        agent_home.join(session_dir)
    };
    contained_existing_path(&root, &candidate)
}

impl AgentAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["kimi-code", "kimi-code-cli"]
    }

    fn display_name(&self) -> &'static str {
        "Kimi Code CLI"
    }

    fn model_providers(&self) -> Vec<ModelProvider> {
        vec![ModelProvider {
            id: "kimi-code".into(),
            display_name: "Kimi Code configured provider catalog".into(),
            local: false,
        }]
    }

    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            installation_detection: Self::partial(
                "version plus Kimi Code help signature; contract-tested, live CLI pending",
            ),
            version_detection: Self::partial("`kimi --version`; live CLI pending"),
            auth_status: Self::unknown(
                "no credential-validating status command is documented; secret-bearing config is never parsed",
            ),
            login: Self::partial("provider-owned `kimi login` device-code flow; live QA pending"),
            logout: Self::unavailable(
                "logout is exposed by the TUI or ACP, not a shell subcommand",
            ),
            multiple_profiles: Self::partial(
                "KIMI_CODE_HOME relocates config, credentials, and sessions; local identity QA is pending",
            ),
            native_resume: Self::partial(
                "`kimi --session <id>` is documented; authenticated continuity QA is pending",
            ),
            session_listing: Self::partial(
                "contract-tested bounded parsing of documented session_index.jsonl and state.json metadata",
            ),
            named_sessions: Self::partial("session state contains provider-generated titles"),
            context_reporting: Self::unavailable(
                "no supported external context-utilization status command was verified",
            ),
            usage_reporting: Self::unavailable("provider quota is not exposed by the adapter"),
            model_reporting: Self::unavailable("current model is session-scoped"),
            model_selection: Self::partial("`--model <alias>`; live QA pending"),
            available_models: Self::unavailable(
                "provider-list JSON may contain plaintext API keys and is intentionally not ingested",
            ),
            multiple_model_providers: Self::partial(
                "documented user-configured Kimi, Anthropic, OpenAI, and Google-compatible providers; live QA pending",
            ),
            programmatic_interface: Self::partial(
                "documented stream-JSON prompt mode and native ACP; live QA pending",
            ),
            non_interactive_mode: Self::partial("`kimi --prompt <prompt>`; live QA pending"),
            structured_output: Self::partial("`--output-format stream-json`; live output pending"),
            acp: Self::partial("documented `kimi acp`; live protocol handshake pending"),
            mcp: Self::partial("documented MCP configuration and ACP forwarding; live QA pending"),
            portable_handoff: Self::partial(
                "the handoff directory is mounted into a new interactive session; Kimi has no safe interactive initial-prompt flag",
            ),
            cloud_handoff: Self::unavailable(
                "ContextWake does not start Kimi's optional local web service or upload handoffs",
            ),
            local_models: Self::partial(
                "custom OpenAI-compatible endpoints are configurable; ContextWake is not an inference runtime",
            ),
            profile_isolation: Self::partial(
                "official data layout places OAuth credentials under KIMI_CODE_HOME; real multi-identity QA is pending",
            ),
        }
    }

    fn acp_transport(&self, agent_home: Option<&Path>) -> Option<AcpTransport> {
        let mut transport = AcpTransport::new(&self.executable).argument("acp");
        if let Some(home) = agent_home {
            transport = transport.environment("KIMI_CODE_HOME", home.as_os_str());
        }
        Some(transport)
    }

    fn detect(&self, agent_home: Option<&Path>) -> Result<AgentHealth> {
        let mut version_command = self.base_command(None);
        version_command.arg("--version");
        let version = run_probe(version_command, PROBE_TIMEOUT);
        let Ok(version) = version else {
            return Ok(AgentHealth {
                agent_id: self.id().into(),
                installed: false,
                executable: None,
                version: None,
                auth_state: AuthState::Unknown,
                message: "Kimi Code CLI could not be found. Install the official `kimi` CLI or set CONTEXTWAKE_KIMI_BIN.".into(),
            });
        };
        if !version.success {
            return Ok(AgentHealth {
                agent_id: self.id().into(),
                installed: false,
                executable: Some(self.executable.clone()),
                version: None,
                auth_state: AuthState::Unknown,
                message: if version.timed_out {
                    "Kimi Code version probe timed out after 10 seconds".into()
                } else {
                    safe_probe_diagnostic(&version)
                },
            });
        }
        let version_text = output_text(&version.stdout, &version.stderr);
        let mut help_command = self.base_command(None);
        help_command.arg("--help");
        let help = run_probe(help_command, PROBE_TIMEOUT);
        if !help.as_ref().is_ok_and(|output| {
            output.success
                && has_kimi_signature(&version_text, &output_text(&output.stdout, &output.stderr))
        }) {
            return Ok(AgentHealth {
                agent_id: self.id().into(),
                installed: false,
                executable: Some(self.executable.clone()),
                version: None,
                auth_state: AuthState::Unknown,
                message:
                    "The discovered `kimi` command did not match Kimi Code CLI's help signature"
                        .into(),
            });
        }
        let auth_state = agent_home
            .map(|home| self.auth_status(home))
            .transpose()?
            .unwrap_or(AuthState::Unknown);
        Ok(AgentHealth {
            agent_id: self.id().into(),
            installed: true,
            executable: Some(self.executable.clone()),
            version: Some(safe_agent_text(&version_text)),
            auth_state,
            message: "Kimi Code CLI detected; authentication remains provider-owned".into(),
        })
    }

    fn auth_status(&self, _agent_home: &Path) -> Result<AuthState> {
        Ok(AuthState::Unknown)
    }

    fn initialize_profile_home(&self, agent_home: &Path) -> Result<()> {
        ensure_profile_directory(agent_home, "Kimi Code")
    }

    fn login(&self, agent_home: &Path, _device_auth: bool) -> Result<AuthState> {
        let status = self
            .base_command(Some(agent_home))
            .arg("login")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Kimi login: {error}"))
            })?;
        if status.success() {
            Ok(AuthState::SignedIn)
        } else {
            Err(ContextWakeError::Provider(format!(
                "Kimi login exited with status {status}"
            )))
        }
    }

    fn logout(&self, _agent_home: &Path) -> Result<()> {
        Err(ContextWakeError::CapabilityUnavailable(
            "Kimi Code logout is currently available inside the TUI or ACP; ContextWake will not edit credential files"
                .into(),
        ))
    }

    fn list_sessions(
        &self,
        agent_home: &Path,
        _workspace: &Path,
        max_count: usize,
    ) -> Result<Vec<DiscoveredAgentSession>> {
        let index_path = agent_home.join("session_index.jsonl");
        if !index_path.is_file() {
            return Ok(Vec::new());
        }
        let bytes = read_file_bounded(&index_path, SESSION_INDEX_LIMIT)?;
        let mut seen = HashSet::new();
        let mut sessions = Vec::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            let Ok(record) = serde_json::from_slice::<KimiSessionIndex>(line) else {
                continue;
            };
            let Ok(session_id) = validate_external_reference(&record.session_id) else {
                continue;
            };
            if !seen.insert(session_id.clone()) {
                continue;
            }
            let Some(session_directory) = session_directory(agent_home, &record.session_dir) else {
                continue;
            };
            let state_path = session_directory.join("state.json");
            let state = read_file_bounded(&state_path, SESSION_STATE_LIMIT)
                .ok()
                .and_then(|bytes| parse_session_state(&bytes).ok());
            let metadata_updated = std::fs::metadata(&state_path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(DateTime::<Utc>::from);
            let (title, created_at, updated_at) = state.unwrap_or((None, None, None));
            sessions.push(DiscoveredAgentSession {
                provider_session_id: session_id,
                title,
                workspace_path: Some(record.work_dir),
                created_at,
                updated_at: updated_at.or(metadata_updated),
            });
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        sessions.truncate(max_count.min(PROVIDER_RESULT_LIMIT));
        Ok(sessions)
    }

    fn resume(&self, agent_home: &Path, workspace: &Path, session_id: &str) -> Result<()> {
        let session_id = validate_external_reference(session_id)?;
        let status = self
            .base_command(Some(agent_home))
            .current_dir(workspace)
            .arg("--session")
            .arg(session_id)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Kimi resume: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(ContextWakeError::Provider(format!(
                "Kimi Code could not natively resume this session (status {status}); create a portable handoff instead"
            )))
        }
    }

    fn start_with_handoff(
        &self,
        agent_home: &Path,
        workspace: &Path,
        handoff_directory: &Path,
        _model_provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<()> {
        let context_path = handoff_directory.join("context.md");
        if !context_path.is_file() {
            return Err(ContextWakeError::InvalidData(
                "handoff context.md is unavailable".into(),
            ));
        }
        eprintln!(
            "ContextWake mounted the portable handoff. In Kimi, ask it to read: {}",
            context_path.display()
        );
        let mut command = self.base_command(Some(agent_home));
        command
            .current_dir(workspace)
            .arg("--add-dir")
            .arg(handoff_directory);
        if let Some(model) = model {
            command
                .arg("--model")
                .arg(validate_external_reference(model)?);
        }
        let status = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Kimi Code: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(ContextWakeError::Provider(format!(
                "Kimi Code handoff launch exited with status {status}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_only_the_current_kimi_code_interface() {
        assert!(has_kimi_signature(
            "0.28.0",
            "Kimi Code CLI\n--session [id]\n--output-format stream-json\nacp"
        ));
        assert!(!has_kimi_signature(
            "1.0",
            "Kimi chatbot downloader --output-format text"
        ));
    }

    #[test]
    fn session_index_stays_inside_provider_home() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path().join("home");
        let session = home.join("sessions/wd_fixture/session_123");
        std::fs::create_dir_all(&session).expect("session");
        std::fs::write(
            session.join("state.json"),
            r#"{"title":"CSV export 🔧","createdAt":"2026-09-01T00:00:00Z","updatedAt":"2026-09-02T00:00:00Z","lastPrompt":"not ingested"}"#,
        )
        .expect("state");
        std::fs::write(
            home.join("session_index.jsonl"),
            format!(
                "{{\"sessionId\":\"session_123\",\"sessionDir\":{},\"workDir\":\"C:/work/repo\"}}\n",
                serde_json::to_string(&session).expect("path JSON")
            ),
        )
        .expect("index");
        let sessions = KimiAdapter::with_executable("unused")
            .list_sessions(&home, Path::new("C:/work/repo"), 10)
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id, "session_123");
        assert_eq!(sessions[0].title.as_deref(), Some("CSV export 🔧"));

        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        assert!(session_directory(&home, &outside).is_none());
    }

    #[test]
    fn profile_home_contains_no_contextwake_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path().join("kimi");
        KimiAdapter::with_executable("unused")
            .initialize_profile_home(&home)
            .expect("profile");
        assert!(home.is_dir());
        assert!(!home.join("credentials").exists());
    }

    #[test]
    fn malformed_and_oversized_session_indexes_fail_closed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path().join("kimi");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join("session_index.jsonl"), b"not-json\n").expect("malformed index");
        let sessions = KimiAdapter::with_executable("unused")
            .list_sessions(&home, directory.path(), 10)
            .expect("malformed records are skipped");
        assert!(sessions.is_empty());

        std::fs::write(
            home.join("session_index.jsonl"),
            vec![b'x'; SESSION_INDEX_LIMIT + 1],
        )
        .expect("oversized index");
        assert!(
            KimiAdapter::with_executable("unused")
                .list_sessions(&home, directory.path(), 10)
                .is_err()
        );
    }

    #[test]
    fn missing_executable_is_optional() {
        let health = KimiAdapter::with_executable("contextwake-test-missing-kimi")
            .detect(None)
            .expect("health");
        assert!(!health.installed);
    }
}
