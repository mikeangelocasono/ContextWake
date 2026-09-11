use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    SignedIn,
    SignedOut,
    Unknown,
}

impl AuthState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SignedIn => "signed_in",
            Self::SignedOut => "signed_out",
            Self::Unknown => "unknown",
        }
    }
}

impl From<&str> for AuthState {
    fn from(value: &str) -> Self {
        match value {
            "signed_in" => Self::SignedIn,
            "signed_out" => Self::SignedOut,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    /// The coding CLI this profile launches (for example `codex` or `claude`).
    #[serde(alias = "provider_id")]
    pub agent_id: String,
    /// The model backend selected through the coding agent, when configured.
    pub model_provider_id: Option<String>,
    pub model_preference: Option<String>,
    pub description: Option<String>,
    #[serde(alias = "provider_home")]
    pub agent_home: PathBuf,
    pub account_fingerprint: Option<String>,
    pub auth_state: AuthState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    Trusted,
    Untrusted,
}

impl TrustState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
        }
    }
}

impl From<&str> for TrustState {
    fn from(value: &str) -> Self {
        if value == "trusted" {
            Self::Trusted
        } else {
            Self::Untrusted
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: Uuid,
    pub path: PathBuf,
    pub display_name: String,
    pub trust_state: TrustState,
    pub preferred_agent_id: Option<String>,
    pub preferred_model_provider_id: Option<String>,
    pub preferred_model: Option<String>,
    pub preferred_profile_id: Option<Uuid>,
    pub last_session_id: Option<Uuid>,
    pub git_root: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitSnapshot {
    pub repository_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub detached_head: bool,
    pub head_commit: Option<String>,
    pub dirty: bool,
    pub staged_count: u32,
    pub unstaged_count: u32,
    pub untracked_count: u32,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub additions: u64,
    pub deletions: u64,
    pub last_commit: Option<String>,
    pub timed_out: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuityRiskLevel {
    Current,
    Notice,
    Warning,
}

impl ContinuityRiskLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Notice => "notice",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContinuityRiskStatus {
    pub level: ContinuityRiskLevel,
    pub latest_checkpoint_id: Option<Uuid>,
    pub checkpoint_age_seconds: Option<u64>,
    pub workspace_changed: bool,
    pub observed_changed_files: u32,
    pub observed_diff_lines: u64,
    pub recommendation: String,
    pub basis: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResumeCapability {
    Native,
    HandoffOnly,
    Unknown,
}

impl ResumeCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::HandoffOnly => "handoff_only",
            Self::Unknown => "unknown",
        }
    }
}

impl From<&str> for ResumeCapability {
    fn from(value: &str) -> Self {
        match value {
            "native" => Self::Native,
            "handoff_only" => Self::HandoffOnly,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    pub id: Uuid,
    pub provider_session_id: Option<String>,
    pub title: Option<String>,
    pub workspace_id: Uuid,
    pub profile_id: Option<Uuid>,
    #[serde(alias = "provider_id")]
    pub agent_id: String,
    pub model_provider_id: Option<String>,
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resume_capability: ResumeCapability,
    pub archived: bool,
    pub continuity: ContinuityKind,
}

/// Provider-owned session metadata discovered through a supported coding-agent
/// interface. This is deliberately smaller than [`Session`]: discovery must not
/// imply that `AgentDeck` owns or can mutate the provider's transcript.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiscoveredAgentSession {
    pub provider_session_id: String,
    pub title: Option<String>,
    pub workspace_path: Option<PathBuf>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl DiscoveredAgentSession {
    pub fn normalized_times(&self, observed_at: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        let started_at = self.created_at.or(self.updated_at).unwrap_or(observed_at);
        let last_seen_at = self.updated_at.unwrap_or(started_at).max(started_at);
        (started_at, last_seen_at)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuityKind {
    NativeResume,
    RestoredFromHandoff,
    NewSession,
    Unknown,
}

impl ContinuityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeResume => "native_resume",
            Self::RestoredFromHandoff => "restored_from_handoff",
            Self::NewSession => "new_session",
            Self::Unknown => "unknown",
        }
    }
}

impl From<&str> for ContinuityKind {
    fn from(value: &str) -> Self {
        match value {
            "native_resume" => Self::NativeResume,
            "restored_from_handoff" => Self::RestoredFromHandoff,
            "new_session" => Self::NewSession,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ValidationResult {
    pub command: Vec<String>,
    pub status: ValidationStatus,
    pub observed_at: DateTime<Utc>,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Passed,
    Failed,
    NotRun,
}

impl ValidationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NotRun => "not_run",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionStatus {
    Clean,
    Redacted,
    NeedsReview,
}

impl RedactionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Redacted => "redacted",
            Self::NeedsReview => "needs_review",
        }
    }
}

impl From<&str> for RedactionStatus {
    fn from(value: &str) -> Self {
        match value {
            "clean" => Self::Clean,
            "redacted" => Self::Redacted,
            _ => Self::NeedsReview,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub session_id: Option<Uuid>,
    #[serde(alias = "provider")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub model_provider_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub profile_id: Option<Uuid>,
    pub objective: String,
    pub active_task: Option<String>,
    pub completed: Vec<String>,
    pub decisions: Vec<String>,
    pub pending_tasks: Vec<String>,
    pub known_issues: Vec<String>,
    pub user_notes: Option<String>,
    #[serde(default)]
    pub project_instructions: Option<String>,
    pub files_changed: Vec<PathBuf>,
    pub git: Option<GitSnapshot>,
    pub validation_results: Vec<ValidationResult>,
    pub created_at: DateTime<Utc>,
    pub redaction_status: RedactionStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffManifest {
    pub schema_version: String,
    pub handoff_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub source_tool: String,
    /// Structured source identity in AWHF 1.1 and newer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<HandoffSource>,
    /// AWHF 1.0 compatibility field. New writers leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provider: Option<String>,
    /// Intended target for switch-generated handoffs. Manual exports can remain
    /// destination-neutral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<HandoffDestination>,
    /// Explicitly distinguishes portable project restoration from native resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity_mode: Option<String>,
    pub checkpoint_id: Uuid,
    pub workspace: HandoffWorkspace,
    pub objective: String,
    pub content_files: Vec<ContentFile>,
    pub redactions: BTreeMap<String, u32>,
    pub security_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffSource {
    pub agent_id: String,
    pub model_provider_id: Option<String>,
    pub model: Option<String>,
    pub profile_id: Option<Uuid>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffDestination {
    pub agent_id: String,
    pub model_provider_id: Option<String>,
    pub model: Option<String>,
    pub profile_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffWorkspace {
    pub display_name: String,
    pub path_fingerprint: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContentFile {
    pub path: String,
    pub sha256: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HandoffRecord {
    pub id: Uuid,
    pub checkpoint_id: Uuid,
    pub schema_version: String,
    pub storage_path: PathBuf,
    pub sha256: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMaturity {
    Stable,
    Experimental,
    Heuristic,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Partial,
    Unsupported,
    Unknown,
    Experimental,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Capability {
    pub support: CapabilitySupport,
    pub maturity: CapabilityMaturity,
    pub detail: String,
}

impl Capability {
    pub const fn is_available(&self) -> bool {
        matches!(
            self.support,
            CapabilitySupport::Supported
                | CapabilitySupport::Partial
                | CapabilitySupport::Experimental
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub installation_detection: Capability,
    pub version_detection: Capability,
    pub auth_status: Capability,
    pub login: Capability,
    pub logout: Capability,
    pub multiple_profiles: Capability,
    pub native_resume: Capability,
    pub session_listing: Capability,
    pub named_sessions: Capability,
    pub context_reporting: Capability,
    pub usage_reporting: Capability,
    pub model_reporting: Capability,
    pub model_selection: Capability,
    pub available_models: Capability,
    pub multiple_model_providers: Capability,
    pub programmatic_interface: Capability,
    pub local_models: Capability,
    pub profile_isolation: Capability,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentHealth {
    pub agent_id: String,
    pub installed: bool,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub auth_state: AuthState,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CodingAgent {
    pub id: String,
    pub display_name: String,
    pub adapter_version: String,
    pub detected_version: Option<String>,
    pub executable: Option<PathBuf>,
    pub capabilities: AgentCapabilities,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelProvider {
    pub id: String,
    pub display_name: String,
    pub local: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentModel {
    pub id: String,
    pub display_name: String,
    pub provider_id: String,
    pub agent_id: String,
    pub cost_classification: ModelCostClassification,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelCostClassification {
    Local,
    FreeTier,
    ApiBilled,
    Subscription,
    Organization,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UsageSummary {
    pub provider_usage_available: bool,
    pub provider_usage_message: String,
    pub local_session_count: u64,
    pub local_checkpoint_count: u64,
    pub local_handoff_count: u64,
    pub local_profile_switch_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_session_without_created_time_starts_at_update_time() {
        let observed_at = Utc::now();
        let updated_at = observed_at - chrono::Duration::seconds(5);
        let discovered = DiscoveredAgentSession {
            provider_session_id: "provider-session".into(),
            title: None,
            workspace_path: None,
            created_at: None,
            updated_at: Some(updated_at),
        };

        assert_eq!(
            discovered.normalized_times(observed_at),
            (updated_at, updated_at)
        );
    }

    #[test]
    fn inconsistent_provider_timestamps_are_kept_chronological() {
        let started_at = Utc::now();
        let discovered = DiscoveredAgentSession {
            provider_session_id: "provider-session".into(),
            title: None,
            workspace_path: None,
            created_at: Some(started_at),
            updated_at: Some(started_at - chrono::Duration::seconds(1)),
        };

        assert_eq!(
            discovered.normalized_times(started_at),
            (started_at, started_at)
        );
    }
}
