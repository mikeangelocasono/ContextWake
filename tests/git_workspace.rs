use std::fs;
use std::process::Command;
use std::time::Duration;

use agentdeck::git::GitClient;

fn git(root: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("Git must be available for this integration test");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn captures_clean_and_dirty_repository_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "tests@example.invalid"]);
    git(root, &["config", "user.name", "AgentDeck Tests"]);

    fs::write(root.join("staged.txt"), "initial\n").expect("seed staged");
    fs::write(root.join("unstaged.txt"), "initial\n").expect("seed unstaged");
    git(root, &["add", "staged.txt", "unstaged.txt"]);
    git(root, &["commit", "-m", "initial"]);

    let client = GitClient::new(Duration::from_secs(5));
    let clean = client
        .snapshot(root)
        .expect("snapshot")
        .expect("Git repository");
    assert!(!clean.dirty);
    assert_eq!(clean.branch.as_deref(), Some("main"));
    assert_eq!(clean.last_commit.as_deref(), Some("initial"));

    fs::write(root.join("staged.txt"), "initial\nstaged\n").expect("modify staged");
    git(root, &["add", "staged.txt"]);
    fs::write(root.join("unstaged.txt"), "initial\nunstaged\n").expect("modify unstaged");
    fs::write(root.join("untracked.txt"), "new\n").expect("create untracked");

    let dirty = client
        .snapshot(root)
        .expect("snapshot")
        .expect("Git repository");
    assert!(dirty.dirty);
    assert_eq!(dirty.staged_count, 1);
    assert_eq!(dirty.unstaged_count, 1);
    assert_eq!(dirty.untracked_count, 1);
    assert!(dirty.additions >= 2);

    let files = client.changed_files(root).expect("changed files");
    assert_eq!(
        files,
        vec![
            std::path::PathBuf::from("staged.txt"),
            std::path::PathBuf::from("unstaged.txt"),
            std::path::PathBuf::from("untracked.txt")
        ]
    );
}

#[test]
fn ordinary_directory_is_a_supported_workspace() {
    let directory = tempfile::tempdir().expect("tempdir");
    let client = GitClient::new(Duration::from_secs(5));
    assert!(
        client
            .snapshot(directory.path())
            .expect("snapshot")
            .is_none()
    );
    assert!(
        client
            .changed_files(directory.path())
            .expect("changed files")
            .is_empty()
    );
}
