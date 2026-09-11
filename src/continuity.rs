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
        let objective = objective
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("Continue work in {}", workspace.display_name));
        let checkpoint = checkpoints.create(
            workspace,
            previous.as_ref(),
            None,
            CheckpointInput {
                objective,
                active_task: Some(format!(
                    "Continue from profile {} using an explicit workspace handoff",
                    previous
                        .as_ref()
                        .map_or("unknown", |profile| profile.display_name.as_str())
                )),
                pending_tasks: vec![format!(
                    "Review this handoff after activating profile {}",
                    target.display_name
                )],
                ..CheckpointInput::default()
            },
        )?;
        Some(handoffs.create(&checkpoint, workspace)?)
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
