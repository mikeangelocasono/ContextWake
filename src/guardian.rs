use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::model::{Checkpoint, ContinuityRiskLevel, ContinuityRiskStatus, GitSnapshot};

/// Changed-file paths entering or leaving the dirty set since the checkpoint.
const SIGNIFICANT_FILE_DELTA: usize = 5;
/// Change in diff lines against HEAD since the checkpoint.
const SIGNIFICANT_CHURN_DELTA: u64 = 250;
/// Age after which any drift counts as significant.
const SIGNIFICANT_AGE_SECONDS: u64 = 7_200;

/// Assesses how far the workspace has moved since its latest checkpoint.
///
/// Drift is measured against the checkpoint's own snapshot: which paths
/// entered or left the changed-file set, how much the diff against HEAD grew
/// or shrank, and whether HEAD or the branch moved (new commits or a
/// checkout). Remote-tracking counts (`ahead`/`behind`, which a plain
/// `git fetch` changes) and the textual form of the repository path are not
/// workspace activity and are ignored.
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
    let churn = git.map_or(0, churn);

    let (level, workspace_changed, recommendation) = if git
        .is_some_and(|snapshot| snapshot.timed_out)
    {
        // A timed-out status carries no counts, so comparing it would report
        // drift (or its absence) that was never observed. The changed-file
        // list comes from separate Git commands; when it returned paths, that
        // evidence is still used.
        let file_delta = latest.map(|checkpoint| file_delta(checkpoint, changed_files));
        let old = checkpoint_age_seconds.is_some_and(|age| age >= SIGNIFICANT_AGE_SECONDS);
        match file_delta {
            None if !changed_files.is_empty() => (
                ContinuityRiskLevel::Warning,
                true,
                "Workspace changes have no checkpoint (Git status timed out, but changed files were observed). Create one before switching profiles.",
            ),
            Some(delta) if !changed_files.is_empty() && delta > 0 => (
                if delta >= SIGNIFICANT_FILE_DELTA || old {
                    ContinuityRiskLevel::Warning
                } else {
                    ContinuityRiskLevel::Notice
                },
                true,
                "Git status timed out, but the changed files differ from the latest checkpoint.",
            ),
            Some(_) => (
                ContinuityRiskLevel::Notice,
                false,
                "Git status timed out, so the workspace could not be compared with the latest checkpoint.",
            ),
            None => (
                ContinuityRiskLevel::Notice,
                false,
                "Git status timed out, so workspace changes could not be inspected. Create a checkpoint when meaningful work begins.",
            ),
        }
    } else if let Some(checkpoint) = latest {
        let drift = Drift::measure(checkpoint, git, changed_files);
        let old = checkpoint_age_seconds.is_some_and(|age| age >= SIGNIFICANT_AGE_SECONDS);
        if drift.baseline_unknown {
            (
                ContinuityRiskLevel::Notice,
                true,
                "The latest checkpoint has no complete Git baseline because Git timed out when it was captured. Create a new checkpoint.",
            )
        } else if !drift.changed {
            (
                ContinuityRiskLevel::Current,
                false,
                "The latest checkpoint matches observable Git state.",
            )
        } else if drift.significant || old {
            (
                ContinuityRiskLevel::Warning,
                true,
                "Workspace activity has moved significantly beyond the latest checkpoint.",
            )
        } else {
            (
                ContinuityRiskLevel::Notice,
                true,
                "Workspace state changed after the latest checkpoint.",
            )
        }
    } else if git.is_some_and(|snapshot| snapshot.dirty) || !changed_files.is_empty() {
        (
            ContinuityRiskLevel::Warning,
            true,
            "Workspace changes have no checkpoint. Create one before switching profiles.",
        )
    } else {
        (
            ContinuityRiskLevel::Notice,
            false,
            "No checkpoint exists yet. Create one when meaningful work begins.",
        )
    };
    ContinuityRiskStatus {
        level,
        latest_checkpoint_id,
        checkpoint_age_seconds,
        workspace_changed,
        observed_changed_files: u32::try_from(changed_files.len()).unwrap_or(u32::MAX),
        observed_diff_lines: churn,
        recommendation: recommendation.into(),
        basis: "LOCAL INDICATOR: checkpoint age plus observable Git state; provider context utilization is unavailable".into(),
    }
}

/// Paths that entered or left the changed-file set since the checkpoint.
fn file_delta(checkpoint: &Checkpoint, changed_files: &[PathBuf]) -> usize {
    let before = checkpoint.files_changed.iter().collect::<BTreeSet<_>>();
    let after = changed_files.iter().collect::<BTreeSet<_>>();
    before.symmetric_difference(&after).count()
}

fn churn(snapshot: &GitSnapshot) -> u64 {
    snapshot.additions.saturating_add(snapshot.deletions)
}

/// Difference between a checkpoint's snapshot and the current workspace.
#[derive(Debug, PartialEq, Eq)]
struct Drift {
    changed: bool,
    significant: bool,
    baseline_unknown: bool,
}

impl Drift {
    fn measure(
        checkpoint: &Checkpoint,
        git: Option<&GitSnapshot>,
        changed_files: &[PathBuf],
    ) -> Self {
        let baseline = checkpoint.git.as_ref();
        if baseline.is_some_and(|snapshot| snapshot.timed_out) {
            return Self {
                changed: true,
                significant: false,
                baseline_unknown: true,
            };
        }
        let file_delta = file_delta(checkpoint, changed_files);
        let (state_changed, head_moved, churn_delta) = match (baseline, git) {
            (None, None) => (false, false, 0),
            // The workspace became, or stopped being, a Git repository.
            (Some(_), None) | (None, Some(_)) => (true, true, 0),
            (Some(before), Some(after)) => {
                let head_moved = before.head_commit != after.head_commit
                    || before.branch != after.branch
                    || before.detached_head != after.detached_head;
                let state_changed = head_moved
                    || before.dirty != after.dirty
                    || before.staged_count != after.staged_count
                    || before.unstaged_count != after.unstaged_count
                    || before.untracked_count != after.untracked_count
                    || before.additions != after.additions
                    || before.deletions != after.deletions;
                (
                    state_changed,
                    head_moved,
                    churn(before).abs_diff(churn(after)),
                )
            }
        };
        Self {
            changed: state_changed || file_delta > 0,
            significant: head_moved
                || file_delta >= SIGNIFICANT_FILE_DELTA
                || churn_delta >= SIGNIFICANT_CHURN_DELTA,
            baseline_unknown: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use uuid::Uuid;

    use super::*;
    use crate::model::{RedactionStatus, ValidationResult};

    fn checkpoint(created_at: DateTime<Utc>, git: GitSnapshot) -> Checkpoint {
        checkpoint_with_files(created_at, git, &["src/lib.rs"])
    }

    fn checkpoint_with_files(
        created_at: DateTime<Utc>,
        git: GitSnapshot,
        files: &[&str],
    ) -> Checkpoint {
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
            files_changed: files.iter().map(PathBuf::from).collect(),
            git: Some(git),
            validation_results: Vec::<ValidationResult>::new(),
            created_at,
            redaction_status: RedactionStatus::Clean,
            conversation: None,
        }
    }

    fn files(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn dirty(additions: u64) -> GitSnapshot {
        GitSnapshot {
            repository_root: Some(PathBuf::from("/repo")),
            branch: Some("main".into()),
            head_commit: Some("a".repeat(40)),
            dirty: true,
            unstaged_count: 1,
            additions,
            ..GitSnapshot::default()
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

    #[test]
    fn small_edit_after_a_checkpoint_of_a_large_dirty_tree_is_only_a_notice() {
        let now = Utc::now();
        let dirty_files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"];
        let checkpoint =
            checkpoint_with_files(now - Duration::minutes(5), dirty(400), &dirty_files);
        let status = assess(
            Some(&checkpoint),
            Some(&dirty(401)),
            &files(&dirty_files),
            now,
        );
        assert_eq!(status.level, ContinuityRiskLevel::Notice);
        assert!(status.workspace_changed);
    }

    #[test]
    fn new_commits_that_leave_a_clean_tree_are_significant() {
        let now = Utc::now();
        let before = GitSnapshot {
            repository_root: Some(PathBuf::from("/repo")),
            branch: Some("main".into()),
            head_commit: Some("a".repeat(40)),
            ..GitSnapshot::default()
        };
        let after = GitSnapshot {
            head_commit: Some("b".repeat(40)),
            last_commit: Some("tenth commit".into()),
            ..before.clone()
        };
        let checkpoint = checkpoint_with_files(now - Duration::minutes(30), before, &[]);
        let status = assess(Some(&checkpoint), Some(&after), &[], now);
        assert_eq!(status.level, ContinuityRiskLevel::Warning);
    }

    #[test]
    fn remote_tracking_and_path_form_changes_are_not_workspace_activity() {
        let now = Utc::now();
        let before = GitSnapshot {
            ahead: Some(0),
            behind: Some(0),
            repository_root: Some(PathBuf::from(r"\\?\D:\Work\Repo")),
            ..dirty(10)
        };
        let after = GitSnapshot {
            behind: Some(3),
            repository_root: Some(PathBuf::from(r"D:\Work\Repo")),
            ..before.clone()
        };
        let checkpoint = checkpoint(now - Duration::minutes(5), before);
        let status = assess(
            Some(&checkpoint),
            Some(&after),
            &files(&["src/lib.rs"]),
            now,
        );
        assert_eq!(status.level, ContinuityRiskLevel::Current);
        assert!(!status.workspace_changed);
    }

    #[test]
    fn file_set_and_churn_deltas_are_measured_since_the_checkpoint() {
        let now = Utc::now();
        let checkpoint = checkpoint(now - Duration::minutes(5), dirty(10));
        let five_new = files(&["src/lib.rs", "1.rs", "2.rs", "3.rs", "4.rs", "5.rs"]);
        let status = assess(Some(&checkpoint), Some(&dirty(10)), &five_new, now);
        assert_eq!(status.level, ContinuityRiskLevel::Warning);

        let status = assess(
            Some(&checkpoint),
            Some(&dirty(300)),
            &files(&["src/lib.rs"]),
            now,
        );
        assert_eq!(status.level, ContinuityRiskLevel::Warning);
    }

    #[test]
    fn old_checkpoints_with_any_drift_still_warn() {
        let now = Utc::now();
        let checkpoint = checkpoint(now - Duration::hours(3), dirty(10));
        let status = assess(
            Some(&checkpoint),
            Some(&dirty(11)),
            &files(&["src/lib.rs"]),
            now,
        );
        assert_eq!(status.level, ContinuityRiskLevel::Warning);
        let unchanged = assess(
            Some(&checkpoint),
            Some(&dirty(10)),
            &files(&["src/lib.rs"]),
            now,
        );
        assert_eq!(unchanged.level, ContinuityRiskLevel::Current);
    }

    #[test]
    fn timed_out_git_status_is_never_reported_as_current() {
        let now = Utc::now();
        let checkpoint = checkpoint(now - Duration::minutes(5), GitSnapshot::default());
        let timed_out = GitSnapshot {
            timed_out: true,
            ..GitSnapshot::default()
        };
        let status = assess(Some(&checkpoint), Some(&timed_out), &[], now);
        assert_eq!(status.level, ContinuityRiskLevel::Notice);
        assert!(status.recommendation.contains("timed out"));

        let baseline_timed_out = checkpoint_with_files(now, timed_out, &[]);
        let status = assess(
            Some(&baseline_timed_out),
            Some(&GitSnapshot::default()),
            &[],
            now,
        );
        assert_eq!(status.level, ContinuityRiskLevel::Notice);
        assert!(status.recommendation.contains("no complete Git baseline"));
    }

    #[test]
    fn timed_out_status_still_uses_observed_changed_files() {
        let now = Utc::now();
        let timed_out = GitSnapshot {
            timed_out: true,
            ..GitSnapshot::default()
        };
        let files = [PathBuf::from("a.rs"), PathBuf::from("b.rs")];

        // Known changes with no checkpoint are a warning, as before the
        // timeout handling existed.
        let status = assess(None, Some(&timed_out), &files, now);
        assert_eq!(status.level, ContinuityRiskLevel::Warning);
        assert!(status.workspace_changed);
        assert_eq!(status.observed_changed_files, 2);
        assert!(status.recommendation.contains("no checkpoint"));

        // A changed-file set that differs from the checkpoint is drift.
        let checkpoint = checkpoint_with_files(
            now - Duration::minutes(5),
            GitSnapshot::default(),
            &["a.rs"],
        );
        let status = assess(Some(&checkpoint), Some(&timed_out), &files, now);
        assert_eq!(status.level, ContinuityRiskLevel::Notice);
        assert!(status.workspace_changed);
        let many = (0..=SIGNIFICANT_FILE_DELTA)
            .map(|index| PathBuf::from(format!("file-{index}.rs")))
            .collect::<Vec<_>>();
        let status = assess(Some(&checkpoint), Some(&timed_out), &many, now);
        assert_eq!(status.level, ContinuityRiskLevel::Warning);

        // The same file set gives no evidence either way.
        let same = checkpoint_with_files(
            now - Duration::minutes(5),
            GitSnapshot::default(),
            &["a.rs", "b.rs"],
        );
        let status = assess(Some(&same), Some(&timed_out), &files, now);
        assert_eq!(status.level, ContinuityRiskLevel::Notice);
        assert!(!status.workspace_changed);
        assert!(status.recommendation.contains("could not be compared"));
    }

    #[test]
    fn non_git_workspaces_compare_only_the_changed_file_set() {
        let now = Utc::now();
        let mut checkpoint = checkpoint_with_files(now, GitSnapshot::default(), &[]);
        checkpoint.git = None;
        assert_eq!(
            assess(Some(&checkpoint), None, &[], now).level,
            ContinuityRiskLevel::Current
        );
        let became_repository = assess(Some(&checkpoint), Some(&dirty(1)), &[], now);
        assert_eq!(became_repository.level, ContinuityRiskLevel::Warning);
    }
}
