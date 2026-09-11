use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::model::{Checkpoint, ContinuityRiskLevel, ContinuityRiskStatus, GitSnapshot};

pub fn assess(
    latest: Option<&Checkpoint>,
    git: Option<&GitSnapshot>,
    changed_files: &[PathBuf],
    now: DateTime<Utc>,
) -> ContinuityRiskStatus {
    let latest_checkpoint_id = latest.map(|checkpoint| checkpoint.id);
    let checkpoint_age_seconds = latest.map(|checkpoint| {
        u64::try_from((now - checkpoint.created_at).num_seconds().max(0)).unwrap_or_default()
    });
    let workspace_changed = latest.map_or_else(
        || git.is_some_and(|snapshot| snapshot.dirty) || !changed_files.is_empty(),
        |checkpoint| checkpoint.git.as_ref() != git || checkpoint.files_changed != changed_files,
    );
    let churn = git.map_or(0, |snapshot| snapshot.additions + snapshot.deletions);
    let significant = changed_files.len() >= 5
        || churn >= 250
        || checkpoint_age_seconds.is_some_and(|age| age >= 7_200);

    let (level, recommendation) = match (latest, workspace_changed, significant) {
        (None, true, _) => (
            ContinuityRiskLevel::Warning,
            "Workspace changes have no checkpoint. Create one before switching profiles.".into(),
        ),
        (None, false, _) => (
            ContinuityRiskLevel::Notice,
            "No checkpoint exists yet. Create one when meaningful work begins.".into(),
        ),
        (Some(_), false, _) => (
            ContinuityRiskLevel::Current,
            "The latest checkpoint matches observable Git state.".into(),
        ),
        (Some(_), true, true) => (
            ContinuityRiskLevel::Warning,
            "Workspace activity has moved significantly beyond the latest checkpoint.".into(),
        ),
        (Some(_), true, false) => (
            ContinuityRiskLevel::Notice,
            "Workspace state changed after the latest checkpoint.".into(),
        ),
    };
    ContinuityRiskStatus {
        level,
        latest_checkpoint_id,
        checkpoint_age_seconds,
        workspace_changed,
        observed_changed_files: u32::try_from(changed_files.len()).unwrap_or(u32::MAX),
        observed_diff_lines: churn,
        recommendation,
        basis: "LOCAL INDICATOR: checkpoint age plus observable Git state; provider context utilization is unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use uuid::Uuid;

    use super::*;
    use crate::model::{RedactionStatus, ValidationResult};

    fn checkpoint(created_at: DateTime<Utc>, git: GitSnapshot) -> Checkpoint {
        Checkpoint {
            schema_version: 1,
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            session_id: None,
            agent_id: None,
            model_provider_id: None,
            model: None,
            profile_id: None,
            objective: "test".into(),
            active_task: None,
            completed: Vec::new(),
            decisions: Vec::new(),
            pending_tasks: Vec::new(),
            known_issues: Vec::new(),
            user_notes: None,
            project_instructions: None,
            files_changed: vec![PathBuf::from("src/lib.rs")],
            git: Some(git),
            validation_results: Vec::<ValidationResult>::new(),
            created_at,
            redaction_status: RedactionStatus::Clean,
        }
    }

    #[test]
    fn warns_when_dirty_workspace_has_no_checkpoint() {
        let git = GitSnapshot {
            dirty: true,
            additions: 300,
            ..GitSnapshot::default()
        };
        let status = assess(None, Some(&git), &[PathBuf::from("src/lib.rs")], Utc::now());
        assert_eq!(status.level, ContinuityRiskLevel::Warning);
        assert!(status.recommendation.contains("no checkpoint"));
    }

    #[test]
    fn distinguishes_current_and_drifted_checkpoint() {
        let now = Utc::now();
        let git = GitSnapshot {
            dirty: true,
            additions: 10,
            ..GitSnapshot::default()
        };
        let checkpoint = checkpoint(now - Duration::minutes(10), git.clone());
        let current = assess(
            Some(&checkpoint),
            Some(&git),
            &[PathBuf::from("src/lib.rs")],
            now,
        );
        assert_eq!(current.level, ContinuityRiskLevel::Current);

        let drifted_git = GitSnapshot {
            additions: 20,
            ..git
        };
        let drifted = assess(
            Some(&checkpoint),
            Some(&drifted_git),
            &[PathBuf::from("src/lib.rs")],
            now,
        );
        assert_eq!(drifted.level, ContinuityRiskLevel::Notice);
        assert!(drifted.workspace_changed);
    }
}
