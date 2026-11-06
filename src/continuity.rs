use std::cmp::Reverse;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::checkpoint::{CheckpointInput, CheckpointService};
use crate::error::{ContextWakeError, Result};
use crate::handoff::{HandoffService, VerifiedHandoff, launch_marker, launch_nonce};
use crate::model::{
    CapabilitySupport, Checkpoint, ContinuityKind, ConversationExcerpt, HandoffRecord, Profile,
    ResumeCapability, Session, Workspace,
};
use crate::paths::{canonicalize_simplified, paths_equivalent, simplify_path};
use crate::provider::{AgentAdapter, HandoffLaunch, LaunchRecord, LaunchedSession};
use crate::security::{sanitize_terminal, sanitize_untrusted_label, validate_session_reference};
use crate::store::Store;

/// How many sessions the continue flow asks an agent's local session store
/// for when `ContextWake` has no session of the profile in the workspace.
const RESUME_DISCOVERY_LIMIT: usize = 50;
const SESSION_TITLE_MAX_CHARS: usize = 200;

#[derive(Clone, Debug)]
pub struct SwitchOutcome {
    pub previous: Option<Profile>,
    pub active: Profile,
    pub handoff: Option<HandoffRecord>,
    pub message: String,
}

/// Options of [`switch_profile_with`].
#[derive(Clone, Debug, Default)]
pub struct SwitchRequest {
    /// Create a checkpoint and a handoff for the target profile.
    pub create_handoff: bool,
    pub objective: Option<String>,
    /// The recent conversation of the source (currently active) profile's
    /// agent in the workspace, stored in the switch checkpoint. It is never
    /// copied from an earlier checkpoint.
    pub conversation: Option<ConversationExcerpt>,
    /// With `create_handoff`, create a fresh checkpoint and handoff even when
    /// the target profile is already active (used before a launch).
    pub refresh_when_active: bool,
}

pub fn switch_profile(
    store: &Store,
    checkpoints: &CheckpointService,
    handoffs: &HandoffService,
    target_reference: &str,
    workspace: Option<&Workspace>,
    create_handoff: bool,
    objective: Option<String>,
) -> Result<SwitchOutcome> {
    switch_profile_with(
        store,
        checkpoints,
        handoffs,
        target_reference,
        workspace,
        SwitchRequest {
            create_handoff,
            objective,
            ..SwitchRequest::default()
        },
    )
}

/// Activates the target profile, optionally creating a checkpoint and a
/// handoff for it first (see [`SwitchRequest`]).
pub fn switch_profile_with(
    store: &Store,
    checkpoints: &CheckpointService,
    handoffs: &HandoffService,
    target_reference: &str,
    workspace: Option<&Workspace>,
    request: SwitchRequest,
) -> Result<SwitchOutcome> {
    let previous = store.active_profile()?;
    let target = store.profile(target_reference)?;
    let already_active = previous
        .as_ref()
        .is_some_and(|profile| profile.id == target.id);
    if already_active && !(request.create_handoff && request.refresh_when_active) {
        return Ok(SwitchOutcome {
            previous,
            active: target,
            handoff: None,
            message: "Profile was already active; no session transfer was attempted".into(),
        });
    }

    let created = if request.create_handoff {
        let workspace = workspace.ok_or_else(|| {
            ContextWakeError::InvalidData(
                "a workspace is required to create a switch handoff".into(),
            )
        })?;
        let previous_checkpoint = checkpoints.latest_for_workspace(workspace.id)?;
        // Seeding copies the explicit project state only: validation results
        // and the conversation excerpt describe the moment they were taken.
        let mut input = previous_checkpoint
            .as_ref()
            .map_or_else(CheckpointInput::default, CheckpointInput::from_checkpoint);
        if let Some(objective) = request.objective.filter(|value| !value.trim().is_empty()) {
            input.objective = objective;
        } else if input.objective.trim().is_empty() {
            input.objective = format!("Continue work in {}", workspace.display_name);
        }
        if input.active_task.is_none() {
            input.active_task = Some(format!(
                "Continue from profile {} using an explicit workspace handoff",
                previous
                    .as_ref()
                    .map_or("unknown", |profile| profile.display_name.as_str())
            ));
        }
        input
            .pending_tasks
            .retain(|task| !task.starts_with("Review this handoff after activating profile "));
        let review_task = format!(
            "Review this handoff after activating profile {}",
            target.display_name
        );
        if !input.pending_tasks.contains(&review_task) {
            input.pending_tasks.push(review_task);
        }
        let checkpoint = checkpoints.create_with_conversation(
            workspace,
            previous.as_ref(),
            None,
            input,
            request.conversation,
        )?;
        match handoffs.create_for_destination(&checkpoint, workspace, Some(&target)) {
            Ok(handoff) => Some((checkpoint, handoff)),
            Err(error) => {
                // A switch that did not happen must not leave a checkpoint behind:
                // it would become the workspace's latest checkpoint and carry a
                // review task for an activation that never took place.
                discard_checkpoint(checkpoints, &checkpoint);
                return Err(error);
            }
        }
    } else {
        None
    };

    // Activation is the final mutation. A failed checkpoint/handoff therefore leaves
    // the previously valid active profile untouched, and a failed activation
    // removes the artifacts this switch created.
    if !already_active && let Err(error) = store.set_active_profile(target.id) {
        if let Some((checkpoint, handoff)) = &created {
            discard_handoff(store, handoff);
            discard_checkpoint(checkpoints, checkpoint);
        }
        return Err(error);
    }
    let handoff = created.map(|(_, handoff)| handoff);
    let message = if already_active {
        "Profile was already active; a fresh checkpoint and workspace handoff were created. No session transfer was attempted."
    } else if handoff.is_some() {
        "Profile switched. Continuity was preserved with a workspace handoff; no native session resume was claimed."
    } else {
        "Profile switched explicitly. No provider session was resumed or transferred."
    };
    Ok(SwitchOutcome {
        previous,
        active: target,
        handoff,
        message: message.into(),
    })
}

/// Best-effort removal of a checkpoint created by a switch that failed; the
/// caller reports the original error.
fn discard_checkpoint(checkpoints: &CheckpointService, checkpoint: &Checkpoint) {
    let _ = checkpoints.delete(&checkpoint.id.to_string());
}

/// Best-effort removal of a handoff created by a switch that failed. The
/// directory was created by this switch inside managed storage.
fn discard_handoff(store: &Store, handoff: &HandoffRecord) {
    let _ = std::fs::remove_dir_all(&handoff.storage_path);
    let _ = store.delete_handoff_index(handoff.id);
}

/// A provider session of a profile in a workspace that a launch can resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResumableSession {
    pub provider_session_id: String,
    pub title: Option<String>,
    pub last_activity_at: Option<DateTime<Utc>>,
    /// The `ContextWake` session row this session was found in, if any;
    /// `None` when it was found in the agent's own local session store.
    pub local_session_id: Option<Uuid>,
}

/// How a continue launch reaches the destination agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LaunchPlan {
    /// Natively resume the agent's own earlier session and deliver the
    /// handoff into it.
    ResumeWithHandoff { session: ResumableSession },
    /// Start a new session with the handoff as its first message.
    NewSession {
        /// The session ID offered to agents that accept one.
        new_session_id: String,
        /// Why no session is resumed.
        reason: String,
    },
}

impl LaunchPlan {
    pub const fn resumes(&self) -> bool {
        matches!(self, Self::ResumeWithHandoff { .. })
    }

    pub fn resume_session_id(&self) -> Option<&str> {
        match self {
            Self::ResumeWithHandoff { session } => Some(&session.provider_session_id),
            Self::NewSession { .. } => None,
        }
    }

    pub fn new_session_id(&self) -> Option<&str> {
        match self {
            Self::NewSession { new_session_id, .. } => Some(new_session_id),
            Self::ResumeWithHandoff { .. } => None,
        }
    }

    /// A new-session plan with a fresh session ID.
    pub fn new_session(reason: impl Into<String>) -> Self {
        Self::NewSession {
            new_session_id: Uuid::new_v4().to_string(),
            reason: reason.into(),
        }
    }

    /// The decision as printed before the launch.
    pub fn describe(&self, agent: &str, workspace: &str, handoff_id: Uuid) -> String {
        match self {
            Self::ResumeWithHandoff { session } => format!(
                "Resuming {agent} session {}{} in workspace {workspace} and delivering handoff {handoff_id}.",
                session_label(session),
                session
                    .last_activity_at
                    .map_or_else(String::new, |at| format!(
                        " (last active {})",
                        at.to_rfc3339_opts(SecondsFormat::Secs, true)
                    )),
            ),
            Self::NewSession { reason, .. } => format!(
                "Starting a new {agent} session in workspace {workspace} with handoff {handoff_id} ({reason})."
            ),
        }
    }
}

fn session_label(session: &ResumableSession) -> String {
    let id = sanitize_terminal(&session.provider_session_id);
    match session.title.as_deref() {
        Some(title) => format!("\"{}\" ({id})", sanitize_terminal(title)),
        None => id,
    }
}

/// Decides whether a continue launch resumes the profile's own session in the
/// workspace or starts a new one. It resumes only when `new_session` is not
/// requested, the agent's native resume is verified, and a provider session
/// of this profile in this workspace is known: the most recently seen
/// `ContextWake` session row, else the newest session in the agent's own
/// local session store. Reading those stores never fails the launch.
pub fn plan_launch(
    store: &Store,
    adapter: &dyn AgentAdapter,
    profile: &Profile,
    workspace: &Workspace,
    new_session: bool,
) -> LaunchPlan {
    if new_session {
        return LaunchPlan::new_session("--new-session was requested");
    }
    let native_resume = adapter.capabilities().native_resume;
    if native_resume.support != CapabilitySupport::Verified {
        return LaunchPlan::new_session(format!(
            "{} native resume is not verified: {}",
            adapter.display_name(),
            native_resume.detail.trim_end_matches('.')
        ));
    }
    match recorded_session(store, profile, workspace)
        .or_else(|| discovered_session(adapter, profile, workspace))
    {
        Some(session) => LaunchPlan::ResumeWithHandoff { session },
        None => LaunchPlan::new_session(format!(
            "no earlier {} session of profile {} is known in this workspace",
            adapter.display_name(),
            profile.display_name
        )),
    }
}

fn recorded_session(
    store: &Store,
    profile: &Profile,
    workspace: &Workspace,
) -> Option<ResumableSession> {
    store
        .list_sessions(false)
        .ok()?
        .into_iter()
        .filter(|session| {
            session.profile_id == Some(profile.id)
                && session.workspace_id == workspace.id
                && session.agent_id == profile.agent_id
        })
        .filter_map(|session| {
            let id = validate_session_reference(session.provider_session_id.as_deref()?).ok()?;
            Some((session.last_seen_at, session.id, session.title, id))
        })
        .max_by_key(|(last_seen_at, ..)| *last_seen_at)
        .map(|(last_seen_at, local_id, title, id)| ResumableSession {
            provider_session_id: id,
            title,
            last_activity_at: Some(last_seen_at),
            local_session_id: Some(local_id),
        })
}

fn discovered_session(
    adapter: &dyn AgentAdapter,
    profile: &Profile,
    workspace: &Workspace,
) -> Option<ResumableSession> {
    if !adapter.capabilities().session_listing.is_available() {
        return None;
    }
    let workspace_path =
        canonicalize_simplified(&workspace.path).unwrap_or_else(|_| simplify_path(&workspace.path));
    let mut sessions = adapter
        .list_sessions(&profile.agent_home, &workspace.path, RESUME_DISCOVERY_LIMIT)
        .ok()?
        .into_iter()
        .filter(|item| {
            item.workspace_path.as_deref().is_some_and(|path| {
                let candidate =
                    canonicalize_simplified(path).unwrap_or_else(|_| simplify_path(path));
                paths_equivalent(&candidate, &workspace_path)
            })
        })
        .filter_map(|item| {
            let id = validate_session_reference(&item.provider_session_id).ok()?;
            Some((item.updated_at.or(item.created_at), item.title, id))
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|(at, ..)| Reverse(*at));
    sessions
        .into_iter()
        .next()
        .map(|(last_activity_at, title, id)| ResumableSession {
            provider_session_id: id,
            title: title
                .as_deref()
                .and_then(|title| sanitize_untrusted_label(title, SESSION_TITLE_MAX_CHARS)),
            last_activity_at,
            local_session_id: None,
        })
}

/// The result of a continue launch.
#[derive(Clone, Debug, Serialize)]
pub struct ContinueOutcome {
    pub handoff_id: Uuid,
    pub workspace: Workspace,
    pub profile_id: Uuid,
    pub agent_id: String,
    pub plan: LaunchPlan,
    /// The `ContextWake` session row written for this launch, if any.
    pub session: Option<Session>,
    /// A user message containing this launch's marker was found in the
    /// agent's transcript.
    pub delivered: bool,
    pub message: String,
}

/// Records what a finished launch did, using only what the agent's own
/// session store confirms. `launched` is the result of
/// [`AgentAdapter::launched_session`].
///
/// * Delivery confirmed: continuity `resumed_with_handoff` (resume) or
///   `restored_from_handoff` (new session).
/// * Otherwise continuity `unknown`.
/// * `resume_capability` is `native` only for a known provider session ID
///   with verified native resume.
///
/// A known provider session that `ContextWake` already indexed for this
/// profile is updated instead of duplicated. Returns the recorded row and
/// whether delivery was confirmed.
pub fn record_launch(
    store: &Store,
    adapter: &dyn AgentAdapter,
    profile: &Profile,
    workspace: &Workspace,
    plan: &LaunchPlan,
    launched: Option<&LaunchedSession>,
) -> Result<(Session, bool)> {
    let delivered = launched.is_some_and(|session| session.prompt_delivered);
    let provider_session_id = launched
        .and_then(|session| validate_session_reference(&session.provider_session_id).ok())
        .or_else(|| plan.resume_session_id().map(str::to_string));
    let continuity = match (delivered, plan.resumes()) {
        (true, true) => ContinuityKind::ResumedWithHandoff,
        (true, false) => ContinuityKind::RestoredFromHandoff,
        (false, _) => ContinuityKind::Unknown,
    };
    let native_resume = adapter.capabilities().native_resume.support;
    let resume_capability = match (&provider_session_id, native_resume) {
        (Some(_), CapabilitySupport::Verified) => ResumeCapability::Native,
        (Some(_), CapabilitySupport::Unsupported) => ResumeCapability::HandoffOnly,
        _ => ResumeCapability::Unknown,
    };
    let title = launched
        .and_then(|session| session.title.clone())
        .or_else(|| match plan {
            LaunchPlan::ResumeWithHandoff { session } => session.title.clone(),
            LaunchPlan::NewSession { .. } => None,
        })
        .as_deref()
        .and_then(|title| sanitize_untrusted_label(title, SESSION_TITLE_MAX_CHARS));
    let now = Utc::now();
    let last_seen_at = launched
        .and_then(|session| session.last_activity_at)
        .map_or(now, |at| at.max(now));
    let session = Session {
        id: Uuid::new_v4(),
        provider_session_id: provider_session_id.clone(),
        title,
        workspace_id: workspace.id,
        profile_id: Some(profile.id),
        agent_id: profile.agent_id.clone(),
        model_provider_id: profile.model_provider_id.clone(),
        model: profile.model_preference.clone(),
        started_at: now,
        last_seen_at,
        resume_capability,
        archived: false,
        continuity,
    };
    let Some(provider_session_id) = provider_session_id else {
        store.insert_session(&session)?;
        return Ok((session, delivered));
    };
    let existing = store.list_sessions(true)?.into_iter().find(|candidate| {
        candidate.agent_id == profile.agent_id
            && candidate.profile_id == Some(profile.id)
            && candidate.provider_session_id.as_deref() == Some(provider_session_id.as_str())
    });
    let Some(existing) = existing else {
        store.upsert_provider_session(&session)?;
        return Ok((session, delivered));
    };
    store.upsert_provider_session(&Session {
        started_at: existing.started_at.min(now),
        title: session.title.or(existing.title),
        ..session
    })?;
    let updated = store.update_session_continuity(existing.id, continuity)?;
    Ok((updated, delivered))
}

/// Everything [`continue_with_handoff`] needs.
pub struct ContinueRequest<'a> {
    pub store: &'a Store,
    pub handoffs: &'a HandoffService,
    /// The active profile's agent.
    pub adapter: &'a dyn AgentAdapter,
    /// The active (destination) profile.
    pub profile: &'a Profile,
    pub workspace: &'a Workspace,
    /// Terminal-safe name and path of the workspace for messages.
    pub workspace_label: &'a str,
    /// The handoff, validated immediately before this call.
    pub handoff: &'a VerifiedHandoff,
    /// The local checkpoint the handoff was rendered from, if readable.
    pub checkpoint: Option<&'a Checkpoint>,
    /// Never resume; always start a new session.
    pub new_session: bool,
}

/// Lines [`continue_with_handoff`] reports while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinueNotice {
    Progress(String),
    Warning(String),
}

/// Runs the continue flow (plan C3): decides between resume and a new
/// session ([`plan_launch`]) and reports the decision before anything is
/// started; builds the bounded launch prompt with a fresh delivery marker;
/// runs the agent; then asks the agent's own session store what the launch
/// created and records only that ([`record_launch`]).
///
/// When the adapter refuses to resume with a handoff before starting
/// anything, the handoff is delivered into a new session instead, and the
/// new decision is reported. A launch that fails records nothing unless the
/// agent's session store shows the session it created.
pub fn continue_with_handoff(
    request: &ContinueRequest<'_>,
    report: &mut dyn FnMut(ContinueNotice),
) -> Result<ContinueOutcome> {
    let ContinueRequest {
        store,
        handoffs,
        adapter,
        profile,
        workspace,
        workspace_label,
        handoff,
        checkpoint,
        new_session,
    } = *request;
    let agent = adapter.display_name();
    let handoff_id = handoff.record.id;
    let mut plan = plan_launch(store, adapter, profile, workspace, new_session);
    report(ContinueNotice::Progress(plan.describe(
        agent,
        workspace_label,
        handoff_id,
    )));
    let marker = launch_marker(handoff_id, &launch_nonce());
    let launch = |plan: &LaunchPlan| -> Result<()> {
        let prompt = handoffs.launch_prompt(handoff, checkpoint, plan.resumes(), &marker)?;
        adapter.launch_with_handoff(
            &profile.agent_home,
            &workspace.path,
            &HandoffLaunch {
                handoff_directory: simplify_path(&handoff.record.storage_path),
                prompt,
                marker: marker.clone(),
                model_provider: profile.model_provider_id.clone(),
                model: profile.model_preference.clone(),
                resume_session_id: plan.resume_session_id().map(str::to_string),
                new_session_id: plan.new_session_id().map(str::to_string),
            },
        )
    };
    let started_at = Utc::now();
    let mut result = launch(&plan);
    if plan.resumes()
        && let Err(ContextWakeError::CapabilityUnavailable(reason)) = &result
    {
        plan = LaunchPlan::new_session(format!(
            "resuming with a handoff is unavailable: {}",
            sanitize_terminal(reason).trim_end_matches('.')
        ));
        report(ContinueNotice::Progress(plan.describe(
            agent,
            workspace_label,
            handoff_id,
        )));
        result = launch(&plan);
    }
    let record = LaunchRecord {
        started_at,
        marker: marker.clone(),
        expected_session_id: plan
            .resume_session_id()
            .or_else(|| plan.new_session_id())
            .map(str::to_string),
    };
    let launched = adapter
        .launched_session(&profile.agent_home, &workspace.path, &record)
        .unwrap_or_else(|error| {
            report(ContinueNotice::Warning(format!(
                "{agent}'s local sessions could not be read to confirm the launch: {}",
                sanitize_terminal(&error.to_string())
            )));
            None
        });
    if let Err(error) = result {
        if let Some(found) = &launched {
            let (session, delivered) =
                record_launch(store, adapter, profile, workspace, &plan, Some(found))?;
            report(ContinueNotice::Progress(launch_result_message(
                agent,
                workspace_label,
                handoff_id,
                &plan,
                &session,
                delivered,
            )));
        }
        return Err(error);
    }
    let (session, delivered) =
        record_launch(store, adapter, profile, workspace, &plan, launched.as_ref())?;
    let message = launch_result_message(
        agent,
        workspace_label,
        handoff_id,
        &plan,
        &session,
        delivered,
    );
    report(ContinueNotice::Progress(message.clone()));
    Ok(ContinueOutcome {
        handoff_id,
        workspace: workspace.clone(),
        profile_id: profile.id,
        agent_id: profile.agent_id.clone(),
        plan,
        session: Some(session),
        delivered,
        message,
    })
}

/// The final message of a continue launch; it never claims more than the
/// agent's transcript confirms.
pub fn launch_result_message(
    agent: &str,
    workspace: &str,
    handoff_id: Uuid,
    plan: &LaunchPlan,
    session: &Session,
    delivered: bool,
) -> String {
    let id = session
        .provider_session_id
        .as_deref()
        .map_or_else(|| "(session ID unknown)".to_string(), sanitize_terminal);
    match (delivered, plan.resumes()) {
        (true, true) => format!(
            "{}: resumed {agent} session {id} in workspace {workspace}; its transcript confirms that it received handoff {handoff_id}.",
            ContinuityKind::ResumedWithHandoff.label()
        ),
        (true, false) => format!(
            "{}: started a new {agent} session {id} in workspace {workspace}; its transcript confirms that it received handoff {handoff_id}. This was not a native session resume.",
            ContinuityKind::RestoredFromHandoff.label()
        ),
        (false, resumed) => format!(
            "Could not confirm that {agent} received the handoff {handoff_id}: no message with this launch's marker was found in its local transcripts. {} in workspace {workspace} was recorded with continuity Unknown.",
            if resumed {
                format!("The resumed session {id}")
            } else if session.provider_session_id.is_some() {
                format!("The new session {id}")
            } else {
                "The launch".to_string()
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::git::GitClient;
    use crate::model::{AuthState, TrustState};
    use crate::paths::AppPaths;

    struct Fixture {
        root: tempfile::TempDir,
        paths: AppPaths,
        store: Store,
        personal: Profile,
        workspace: Workspace,
        checkpoints: CheckpointService,
        source: Checkpoint,
    }

    fn profile(paths: &AppPaths, name: &str) -> Profile {
        let now = Utc::now();
        let id = Uuid::new_v4();
        Profile {
            id,
            name: name.into(),
            display_name: name.into(),
            agent_id: "codex".into(),
            model_provider_id: None,
            model_preference: None,
            description: None,
            agent_home: paths.agent_home("codex", &id),
            home_mode: crate::model::ProfileHomeMode::Isolated,
            account_fingerprint: None,
            auth_state: AuthState::SignedOut,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        }
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path().join("state"));
        paths.ensure().expect("paths");
        let store = Store::open(paths.state_db()).expect("store");
        let personal = profile(&paths, "personal");
        let work = profile(&paths, "work");
        store.insert_profile(&personal).expect("personal");
        store.insert_profile(&work).expect("work");
        store.set_active_profile(personal.id).expect("activate");
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: root.path().join("workspace"),
            display_name: "workspace".into(),
            trust_state: TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        };
        std::fs::create_dir_all(&workspace.path).expect("workspace");
        store.upsert_workspace(&workspace).expect("register");
        let checkpoints = CheckpointService::new(
            store.clone(),
            paths.clone(),
            GitClient::new(Duration::from_secs(10)),
        );
        let source = checkpoints
            .create(
                &workspace,
                Some(&personal),
                None,
                CheckpointInput {
                    objective: "Implement checked division".into(),
                    pending_tasks: vec!["Division tests".into()],
                    ..CheckpointInput::default()
                },
            )
            .expect("source checkpoint");
        Fixture {
            root,
            paths,
            store,
            personal,
            workspace,
            checkpoints,
            source,
        }
    }

    fn assert_only_source_checkpoint_remains(fixture: &Fixture) {
        let indexed = fixture.store.list_checkpoint_paths().expect("index");
        assert_eq!(
            indexed.iter().map(|row| row.0).collect::<Vec<_>>(),
            vec![fixture.source.id],
            "the failed switch's checkpoint must be removed from the index"
        );
        let files = std::fs::read_dir(fixture.paths.checkpoints_dir())
            .expect("checkpoint directory")
            .count();
        assert_eq!(
            files, 1,
            "the failed switch's checkpoint file must be removed"
        );
        assert_eq!(
            fixture
                .checkpoints
                .latest_for_workspace(fixture.workspace.id)
                .expect("latest")
                .map(|checkpoint| checkpoint.id),
            Some(fixture.source.id)
        );
        assert_eq!(
            fixture
                .store
                .active_profile()
                .expect("active")
                .expect("profile")
                .id,
            fixture.personal.id
        );
    }

    #[test]
    fn failed_handoff_creation_removes_the_switch_checkpoint() {
        let fixture = fixture();
        // Handoff storage that does not exist makes handoff creation fail after
        // the switch checkpoint has been written.
        let unavailable_handoffs = HandoffService::new(
            fixture.store.clone(),
            AppPaths::from_root(fixture.root.path().join("missing-state")),
        );
        let result = switch_profile(
            &fixture.store,
            &fixture.checkpoints,
            &unavailable_handoffs,
            "work",
            Some(&fixture.workspace),
            true,
            None,
        );
        assert!(result.is_err());
        assert_only_source_checkpoint_remains(&fixture);
    }

    #[test]
    fn failed_activation_removes_the_switch_checkpoint_and_handoff() {
        let fixture = fixture();
        let handoffs = HandoffService::new(fixture.store.clone(), fixture.paths.clone());
        // The final activation fails after the checkpoint and handoff exist.
        rusqlite::Connection::open(fixture.store.path())
            .expect("database")
            .execute_batch(
                "CREATE TRIGGER reject_profile_switch BEFORE UPDATE ON settings
                 WHEN NEW.key = 'active_profile_id'
                 BEGIN SELECT RAISE(ABORT, 'injected activation failure'); END;",
            )
            .expect("trigger");
        let result = switch_profile(
            &fixture.store,
            &fixture.checkpoints,
            &handoffs,
            "work",
            Some(&fixture.workspace),
            true,
            Some("Continue division".into()),
        );
        assert!(result.is_err());
        assert!(fixture.store.list_handoffs().expect("handoffs").is_empty());
        assert_eq!(
            std::fs::read_dir(fixture.paths.handoffs_dir())
                .expect("handoff directory")
                .count(),
            0,
            "the failed switch's handoff directory must be removed"
        );
        assert_only_source_checkpoint_remains(&fixture);
    }

    // ---- Continue flow (plan C3) with a fake agent ----

    use std::path::Path;
    use std::sync::Mutex;

    use crate::model::{
        AgentCapabilities, AgentHealth, CapabilitySupport, DiscoveredAgentSession, ModelProvider,
    };
    use crate::provider::{AgentAdapter, CursorAdapter, HandoffLaunch, LaunchRecord};

    /// What the fake agent's session store shows after a launch.
    #[derive(Clone)]
    enum Found {
        Nothing,
        /// The session the launch asked for (resumed or pre-assigned).
        Expected {
            delivered: bool,
        },
        Other {
            id: String,
            delivered: bool,
        },
    }

    struct FakeAgent {
        native_resume: CapabilitySupport,
        listed: Vec<DiscoveredAgentSession>,
        refuse_resume: bool,
        fail_launch: bool,
        found: Found,
        launches: Mutex<Vec<HandoffLaunch>>,
        records: Mutex<Vec<LaunchRecord>>,
    }

    impl FakeAgent {
        fn new(native_resume: CapabilitySupport, found: Found) -> Self {
            Self {
                native_resume,
                listed: Vec::new(),
                refuse_resume: false,
                fail_launch: false,
                found,
                launches: Mutex::new(Vec::new()),
                records: Mutex::new(Vec::new()),
            }
        }

        fn launches(&self) -> Vec<HandoffLaunch> {
            self.launches.lock().expect("launches").clone()
        }
    }

    impl AgentAdapter for FakeAgent {
        fn id(&self) -> &'static str {
            "codex"
        }
        fn display_name(&self) -> &'static str {
            "Fake Agent"
        }
        fn model_providers(&self) -> Vec<ModelProvider> {
            Vec::new()
        }
        fn capabilities(&self) -> AgentCapabilities {
            let mut capabilities = CursorAdapter::with_executable("unused-agent").capabilities();
            capabilities.native_resume.support = self.native_resume;
            capabilities.native_resume.detail = "tested fake".into();
            capabilities.session_listing.support = CapabilitySupport::Verified;
            capabilities
        }
        fn detect(&self, _agent_home: Option<&Path>) -> Result<AgentHealth> {
            Err(ContextWakeError::CapabilityUnavailable("fake".into()))
        }
        fn auth_status(&self, _agent_home: &Path) -> Result<AuthState> {
            Ok(AuthState::Unknown)
        }
        fn initialize_profile_home(&self, _agent_home: &Path) -> Result<()> {
            Ok(())
        }
        fn login(&self, _agent_home: &Path, _device_auth: bool) -> Result<AuthState> {
            Err(ContextWakeError::CapabilityUnavailable("fake".into()))
        }
        fn logout(&self, _agent_home: &Path) -> Result<()> {
            Err(ContextWakeError::CapabilityUnavailable("fake".into()))
        }
        fn list_sessions(
            &self,
            _agent_home: &Path,
            _workspace: &Path,
            _max_count: usize,
        ) -> Result<Vec<DiscoveredAgentSession>> {
            Ok(self.listed.clone())
        }
        fn resume(&self, _agent_home: &Path, _workspace: &Path, _session_id: &str) -> Result<()> {
            Err(ContextWakeError::CapabilityUnavailable("fake".into()))
        }
        fn start_with_handoff(
            &self,
            _agent_home: &Path,
            _workspace: &Path,
            _handoff_directory: &Path,
            _model_provider: Option<&str>,
            _model: Option<&str>,
        ) -> Result<()> {
            Err(ContextWakeError::CapabilityUnavailable("fake".into()))
        }
        fn launch_with_handoff(
            &self,
            _agent_home: &Path,
            _workspace: &Path,
            launch: &HandoffLaunch,
        ) -> Result<()> {
            if self.refuse_resume && launch.resume_session_id.is_some() {
                return Err(ContextWakeError::CapabilityUnavailable(
                    "Fake Agent cannot deliver a handoff into a resumed session".into(),
                ));
            }
            self.launches.lock().expect("launches").push(launch.clone());
            if self.fail_launch {
                return Err(ContextWakeError::Provider(
                    "Fake Agent exited with status 1".into(),
                ));
            }
            Ok(())
        }
        fn launched_session(
            &self,
            _agent_home: &Path,
            _workspace: &Path,
            record: &LaunchRecord,
        ) -> Result<Option<LaunchedSession>> {
            self.records.lock().expect("records").push(record.clone());
            let sent = self
                .launches()
                .last()
                .is_some_and(|launch| launch.prompt.contains(&record.marker));
            Ok(match &self.found {
                Found::Nothing => None,
                Found::Expected { delivered } => Some(LaunchedSession {
                    provider_session_id: record.expected_session_id.clone().expect("expected id"),
                    title: Some("Fix the parser".into()),
                    last_activity_at: Some(Utc::now()),
                    prompt_delivered: *delivered && sent,
                }),
                Found::Other { id, delivered } => Some(LaunchedSession {
                    provider_session_id: id.clone(),
                    title: None,
                    last_activity_at: None,
                    prompt_delivered: *delivered && sent,
                }),
            })
        }
    }

    fn session_row(fixture: &Fixture, provider_id: &str, minutes_ago: i64) -> Session {
        let seen = Utc::now() - chrono::Duration::minutes(minutes_ago);
        Session {
            id: Uuid::new_v4(),
            provider_session_id: Some(provider_id.into()),
            title: Some(format!("title {provider_id}")),
            workspace_id: fixture.workspace.id,
            profile_id: Some(fixture.personal.id),
            agent_id: "codex".into(),
            model_provider_id: None,
            model: None,
            started_at: seen,
            last_seen_at: seen,
            resume_capability: ResumeCapability::Native,
            archived: false,
            continuity: ContinuityKind::Unknown,
        }
    }

    struct Run {
        outcome: Result<ContinueOutcome>,
        notices: Vec<ContinueNotice>,
    }

    fn run_continue(fixture: &Fixture, agent: &FakeAgent, new_session: bool) -> Run {
        let handoffs = HandoffService::new(fixture.store.clone(), fixture.paths.clone());
        let record = handoffs
            .create(&fixture.source, &fixture.workspace)
            .expect("handoff");
        let verified = handoffs.verify(&record.id.to_string()).expect("verified");
        let mut notices = Vec::new();
        let outcome = continue_with_handoff(
            &ContinueRequest {
                store: &fixture.store,
                handoffs: &handoffs,
                adapter: agent,
                profile: &fixture.personal,
                workspace: &fixture.workspace,
                workspace_label: "workspace (D:\\Mike Angelo Work Files\\app)",
                handoff: &verified,
                checkpoint: Some(&fixture.source),
                new_session,
            },
            &mut |notice| notices.push(notice),
        );
        Run { outcome, notices }
    }

    fn progress(run: &Run) -> Vec<String> {
        run.notices
            .iter()
            .filter_map(|notice| match notice {
                ContinueNotice::Progress(line) => Some(line.clone()),
                ContinueNotice::Warning(_) => None,
            })
            .collect()
    }

    #[test]
    fn resume_plan_prefers_the_newest_recorded_session_of_this_profile_and_workspace() {
        let fixture = fixture();
        let agent = FakeAgent::new(CapabilitySupport::Verified, Found::Nothing);
        fixture
            .store
            .insert_session(&session_row(&fixture, "older", 30))
            .expect("older");
        fixture
            .store
            .insert_session(&session_row(&fixture, "newest", 5))
            .expect("newest");
        // Another profile's newer session and another workspace's are ignored.
        let foreign_profile = Session {
            profile_id: None,
            ..session_row(&fixture, "orphan", 1)
        };
        fixture
            .store
            .insert_session(&foreign_profile)
            .expect("foreign");
        let plan = plan_launch(
            &fixture.store,
            &agent,
            &fixture.personal,
            &fixture.workspace,
            false,
        );
        let LaunchPlan::ResumeWithHandoff { session } = &plan else {
            panic!("expected a resume plan, got {plan:?}");
        };
        assert_eq!(session.provider_session_id, "newest");
        assert!(session.local_session_id.is_some());
        assert!(
            plan.describe("Fake Agent", "app", Uuid::nil())
                .starts_with("Resuming Fake Agent session \"title newest\" (newest) (last active ")
        );

        // --new-session and unverified native resume always start a new session.
        let forced = plan_launch(
            &fixture.store,
            &agent,
            &fixture.personal,
            &fixture.workspace,
            true,
        );
        assert!(!forced.resumes());
        assert!(
            forced
                .describe("Fake Agent", "app", Uuid::nil())
                .contains("--new-session")
        );
        let partial = FakeAgent::new(CapabilitySupport::Partial, Found::Nothing);
        let plan = plan_launch(
            &fixture.store,
            &partial,
            &fixture.personal,
            &fixture.workspace,
            false,
        );
        assert!(
            matches!(&plan, LaunchPlan::NewSession { reason, .. } if reason.contains("native resume is not verified")),
            "{plan:?}"
        );
    }

    #[test]
    fn resume_plan_falls_back_to_the_agents_own_session_list_for_this_workspace() {
        let fixture = fixture();
        let mut agent = FakeAgent::new(CapabilitySupport::Verified, Found::Nothing);
        let now = Utc::now();
        agent.listed = vec![
            DiscoveredAgentSession {
                provider_session_id: "other-workspace".into(),
                title: None,
                workspace_path: Some(fixture.root.path().join("elsewhere")),
                created_at: Some(now),
                updated_at: Some(now),
            },
            DiscoveredAgentSession {
                provider_session_id: "older-here".into(),
                title: None,
                workspace_path: Some(fixture.workspace.path.clone()),
                created_at: Some(now - chrono::Duration::hours(2)),
                updated_at: Some(now - chrono::Duration::hours(2)),
            },
            DiscoveredAgentSession {
                provider_session_id: "newest-here".into(),
                title: Some("Parser\nwork".into()),
                workspace_path: Some(fixture.workspace.path.clone()),
                created_at: Some(now - chrono::Duration::hours(1)),
                updated_at: Some(now - chrono::Duration::minutes(1)),
            },
            DiscoveredAgentSession {
                provider_session_id: "../escape".into(),
                title: None,
                workspace_path: Some(fixture.workspace.path.clone()),
                created_at: Some(now),
                updated_at: Some(now),
            },
        ];
        let plan = plan_launch(
            &fixture.store,
            &agent,
            &fixture.personal,
            &fixture.workspace,
            false,
        );
        let LaunchPlan::ResumeWithHandoff { session } = plan else {
            panic!("expected a resume plan");
        };
        assert_eq!(session.provider_session_id, "newest-here");
        assert_eq!(session.title.as_deref(), Some("Parser work"));
        assert_eq!(session.local_session_id, None);

        agent.listed.clear();
        let plan = plan_launch(
            &fixture.store,
            &agent,
            &fixture.personal,
            &fixture.workspace,
            false,
        );
        assert!(
            matches!(&plan, LaunchPlan::NewSession { reason, .. } if reason.contains("no earlier Fake Agent session")),
            "{plan:?}"
        );
    }

    #[test]
    fn confirmed_resume_records_native_resume_with_handoff_without_duplicating_the_row() {
        let fixture = fixture();
        let agent = FakeAgent::new(
            CapabilitySupport::Verified,
            Found::Expected { delivered: true },
        );
        let row = session_row(&fixture, "019a-session", 10);
        fixture.store.insert_session(&row).expect("row");
        let run = run_continue(&fixture, &agent, false);
        let outcome = run.outcome.as_ref().expect("continue");
        assert!(outcome.delivered);
        let launches = agent.launches();
        assert_eq!(launches.len(), 1);
        let launch = &launches[0];
        assert_eq!(launch.resume_session_id.as_deref(), Some("019a-session"));
        assert_eq!(launch.new_session_id, None);
        assert!(
            launch.prompt.starts_with(&launch.marker),
            "marker is the first line"
        );
        assert!(launch.marker.starts_with("ContextWake handoff "));
        assert!(
            launch
                .prompt
                .contains("You are resuming your own earlier session")
        );
        assert!(!launch.prompt.contains('\r'));
        let messages = progress(&run);
        assert!(
            messages[0].starts_with("Resuming Fake Agent session"),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with("Native Resume + Handoff:"),
            "{messages:?}"
        );

        let rows = fixture
            .store
            .list_sessions(true)
            .expect("sessions")
            .into_iter()
            .filter(|session| session.provider_session_id.as_deref() == Some("019a-session"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "the indexed row is updated, not duplicated");
        assert_eq!(rows[0].id, row.id);
        assert_eq!(rows[0].continuity, ContinuityKind::ResumedWithHandoff);
        assert_eq!(rows[0].resume_capability, ResumeCapability::Native);
        let record = agent.records.lock().expect("records")[0].clone();
        assert_eq!(record.expected_session_id.as_deref(), Some("019a-session"));
        assert_eq!(record.marker, launch.marker);
    }

    #[test]
    fn confirmed_new_session_is_restored_from_handoff_with_the_discovered_id() {
        let fixture = fixture();
        let agent = FakeAgent::new(
            CapabilitySupport::Verified,
            Found::Other {
                id: "rollout-1234".into(),
                delivered: true,
            },
        );
        let run = run_continue(&fixture, &agent, true);
        let outcome = run.outcome.expect("continue");
        let launch = agent.launches().remove(0);
        assert_eq!(launch.resume_session_id, None);
        let assigned = launch.new_session_id.clone().expect("new session id");
        assert!(Uuid::parse_str(&assigned).is_ok());
        assert!(launch.prompt.contains("This is a new session."));
        let session = outcome.session.expect("session row");
        assert_eq!(session.provider_session_id.as_deref(), Some("rollout-1234"));
        assert_eq!(session.continuity, ContinuityKind::RestoredFromHandoff);
        assert_eq!(session.resume_capability, ResumeCapability::Native);
        assert!(
            outcome
                .message
                .contains("This was not a native session resume")
        );
        assert_eq!(
            fixture.store.session("rollout-1234").expect("indexed").id,
            session.id
        );
    }

    #[test]
    fn unconfirmed_delivery_is_recorded_as_unknown_and_said_plainly() {
        let fixture = fixture();
        for found in [
            Found::Nothing,
            Found::Other {
                id: "seen-but-no-marker".into(),
                delivered: false,
            },
        ] {
            let agent = FakeAgent::new(CapabilitySupport::Partial, found);
            let run = run_continue(&fixture, &agent, false);
            let outcome = run.outcome.expect("continue");
            assert!(!outcome.delivered);
            let session = outcome.session.expect("an accurate record");
            assert_eq!(session.continuity, ContinuityKind::Unknown);
            assert_ne!(session.resume_capability, ResumeCapability::Native);
            assert!(
                outcome
                    .message
                    .starts_with("Could not confirm that Fake Agent received the handoff"),
                "{}",
                outcome.message
            );
            assert!(!outcome.message.contains("Restored"));
        }
    }

    #[test]
    fn refused_resume_falls_back_to_a_new_session_and_says_so() {
        let fixture = fixture();
        fixture
            .store
            .insert_session(&session_row(&fixture, "resumable", 3))
            .expect("row");
        let mut agent = FakeAgent::new(
            CapabilitySupport::Verified,
            Found::Expected { delivered: true },
        );
        agent.refuse_resume = true;
        let run = run_continue(&fixture, &agent, false);
        let outcome = run.outcome.as_ref().expect("continue");
        assert!(!outcome.plan.resumes());
        let launches = agent.launches();
        assert_eq!(launches.len(), 1, "only the new session was started");
        assert_eq!(launches[0].resume_session_id, None);
        assert!(launches[0].prompt.contains("This is a new session."));
        let messages = progress(&run);
        assert!(messages[0].starts_with("Resuming"));
        assert!(
            messages[1].contains("resuming with a handoff is unavailable"),
            "{messages:?}"
        );
        assert_eq!(
            outcome.session.as_ref().expect("row").continuity,
            ContinuityKind::RestoredFromHandoff
        );
    }

    #[test]
    fn failed_launch_records_nothing_the_agent_store_does_not_show() {
        let fixture = fixture();
        let before = fixture.store.list_sessions(true).expect("sessions").len();
        let mut agent = FakeAgent::new(CapabilitySupport::Verified, Found::Nothing);
        agent.fail_launch = true;
        let run = run_continue(&fixture, &agent, true);
        assert!(matches!(run.outcome, Err(ContextWakeError::Provider(_))));
        assert_eq!(
            fixture.store.list_sessions(true).expect("sessions").len(),
            before
        );

        // When the agent's store shows the session, that is recorded accurately.
        let mut agent = FakeAgent::new(
            CapabilitySupport::Verified,
            Found::Other {
                id: "exited-with-error".into(),
                delivered: true,
            },
        );
        agent.fail_launch = true;
        let run = run_continue(&fixture, &agent, true);
        assert!(run.outcome.is_err());
        let recorded = fixture
            .store
            .session("exited-with-error")
            .expect("recorded");
        assert_eq!(recorded.continuity, ContinuityKind::RestoredFromHandoff);
    }

    #[test]
    fn launch_prompt_passed_to_the_agent_is_bounded_and_points_at_context_md() {
        let fixture = fixture();
        let agent = FakeAgent::new(CapabilitySupport::Partial, Found::Nothing);
        run_continue(&fixture, &agent, false)
            .outcome
            .expect("continue");
        let launch = agent.launches().remove(0);
        assert!(
            launch.prompt.chars().count() <= crate::handoff::LAUNCH_PROMPT_MAX_CHARS,
            "prompt length"
        );
        assert!(!launch.prompt.starts_with('-'));
        let context = launch.handoff_directory.join("context.md");
        assert!(context.is_file());
        assert!(
            launch.prompt.contains(&context.display().to_string()),
            "{}",
            launch.prompt
        );
        assert!(launch.prompt.contains("Implement checked division"));
        assert!(
            !launch
                .handoff_directory
                .to_string_lossy()
                .starts_with(r"\\?\")
        );
    }

    #[test]
    fn refreshing_switch_for_the_active_profile_creates_a_fresh_handoff_with_the_new_excerpt() {
        let fixture = fixture();
        let handoffs = HandoffService::new(fixture.store.clone(), fixture.paths.clone());
        let excerpt = |id: &str, request: &str| ConversationExcerpt {
            agent_id: "codex".into(),
            provider_session_id: id.into(),
            session_title: None,
            captured_at: Utc::now(),
            last_activity_at: None,
            user_requests: vec![request.into()],
            last_agent_message: Some("Done.".into()),
            truncated: false,
        };
        // The active profile switching to itself without refresh does nothing.
        let unchanged = switch_profile_with(
            &fixture.store,
            &fixture.checkpoints,
            &handoffs,
            "personal",
            Some(&fixture.workspace),
            SwitchRequest {
                create_handoff: true,
                conversation: Some(excerpt("first", "Add tests")),
                ..SwitchRequest::default()
            },
        )
        .expect("no-op");
        assert!(unchanged.handoff.is_none());

        let refreshed = switch_profile_with(
            &fixture.store,
            &fixture.checkpoints,
            &handoffs,
            "personal",
            Some(&fixture.workspace),
            SwitchRequest {
                create_handoff: true,
                conversation: Some(excerpt("first", "Add tests")),
                refresh_when_active: true,
                ..SwitchRequest::default()
            },
        )
        .expect("refresh");
        let first = refreshed.handoff.expect("fresh handoff");
        assert!(refreshed.message.contains("already active"));
        let first_checkpoint = fixture
            .checkpoints
            .read(&first.checkpoint_id.to_string())
            .expect("checkpoint");
        assert_eq!(
            first_checkpoint
                .conversation
                .as_ref()
                .map(|value| value.provider_session_id.as_str()),
            Some("first")
        );

        // A later switch without a new excerpt does not inherit the old one.
        let switched = switch_profile_with(
            &fixture.store,
            &fixture.checkpoints,
            &handoffs,
            "work",
            Some(&fixture.workspace),
            SwitchRequest {
                create_handoff: true,
                ..SwitchRequest::default()
            },
        )
        .expect("switch");
        let second = fixture
            .checkpoints
            .read(&switched.handoff.expect("handoff").checkpoint_id.to_string())
            .expect("checkpoint");
        assert_eq!(second.objective, first_checkpoint.objective);
        assert!(second.conversation.is_none(), "the old excerpt was copied");
        assert_eq!(
            fixture
                .store
                .active_profile()
                .expect("active")
                .expect("profile")
                .name,
            "work"
        );
    }
}
