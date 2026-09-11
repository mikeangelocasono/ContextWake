use crate::checkpoint::{CheckpointInput, CheckpointService};
use crate::error::Result;
use crate::handoff::HandoffService;
use crate::model::{HandoffRecord, Profile, Workspace};
use crate::store::Store;

#[derive(Clone, Debug)]
pub struct SwitchOutcome {
    pub previous: Option<Profile>,
    pub active: Profile,
    pub handoff: Option<HandoffRecord>,
    pub message: String,
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
    let previous = store.active_profile()?;
    let target = store.profile(target_reference)?;
    if previous
        .as_ref()
        .is_some_and(|profile| profile.id == target.id)
    {
        return Ok(SwitchOutcome {
            previous,
            active: target,
            handoff: None,
            message: "Profile was already active; no session transfer was attempted".into(),
        });
    }

    let handoff = if create_handoff {
        let workspace = workspace.ok_or_else(|| {
            crate::error::AgentDeckError::InvalidData(
                "a workspace is required to create a switch handoff".into(),
            )
        })?;
        let previous_checkpoint = checkpoints.latest_for_workspace(workspace.id)?;
        let mut input = previous_checkpoint
            .as_ref()
            .map_or_else(CheckpointInput::default, CheckpointInput::from_checkpoint);
        if let Some(objective) = objective.filter(|value| !value.trim().is_empty()) {
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
        let checkpoint = checkpoints.create(workspace, previous.as_ref(), None, input)?;
        Some(handoffs.create_for_destination(&checkpoint, workspace, Some(&target))?)
    } else {
        None
    };

    // Activation is the final mutation. A failed checkpoint/handoff therefore leaves
    // the previously valid active profile untouched.
    store.set_active_profile(target.id)?;
    let message = if handoff.is_some() {
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
