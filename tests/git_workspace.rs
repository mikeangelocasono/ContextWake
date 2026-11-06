use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use contextwake::git::GitClient;
use contextwake::paths::{canonicalize_simplified, paths_equivalent};
use serde_json::Value;

fn git(root: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("Git must be available for this integration test");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(root: &Path, args: &[&str], input: Option<&str>) -> String {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .spawn()
        .expect("Git must be available for this integration test");
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write Git input");
    }
    let output = child.wait_with_output().expect("Git output");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_string()
}

fn init_repository(root: &Path) {
    fs::create_dir_all(root).expect("repository directory");
    git(root, &["init", "-q", "-b", "main"]);
}

fn commit(root: &Path, message: &str) {
    git(
        root,
        &[
            "-c",
            "user.name=ContextWake Tests",
            "-c",
            "user.email=tests@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// The path with forward slashes, as Git's shell, Git configuration values and POSIX
/// tools accept it on every platform.
fn shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Gives `path` an older modification time so Git must re-read it (through its filters)
/// and would refresh, that is rewrite, the index.
fn backdate(path: &Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open file")
        .set_modified(SystemTime::now() - Duration::from_secs(3_600))
        .expect("backdate file");
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("executable script");
    }
}

fn append(path: &Path, text: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open for append")
        .write_all(text.as_bytes())
        .expect("append");
}

fn file_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn ctx(home: &Path) -> assert_cmd::Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("ctx");
    command
        .env("CONTEXTWAKE_HOME", home)
        .env("CONTEXTWAKE_CODEX_BIN", "contextwake-test-missing-codex")
        .env("CONTEXTWAKE_CLAUDE_BIN", "contextwake-test-missing-claude");
    command
}

fn json_status(command: &mut assert_cmd::Command, workspace: &Path) -> Value {
    let output = command
        .arg("--json")
        .arg("status")
        .arg(workspace)
        .output()
        .expect("run ctx status");
    assert!(
        output.status.success(),
        "ctx status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

#[test]
fn captures_clean_and_dirty_repository_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "tests@example.invalid"]);
    git(root, &["config", "user.name", "ContextWake Tests"]);

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

#[test]
fn repository_paths_are_conventional() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path().join("repository");
    init_repository(&root);
    fs::create_dir_all(root.join("nested")).expect("nested directory");

    let client = GitClient::new(Duration::from_secs(10));
    let expected = canonicalize_simplified(&root).expect("canonical root");
    for start in [root.clone(), root.join("nested")] {
        let found = client
            .repository_root(&start)
            .expect("root")
            .expect("repository");
        assert!(!found.to_string_lossy().starts_with(r"\\?\"), "{found:?}");
        assert!(paths_equivalent(&found, &expected), "{found:?}");
    }
    let snapshot = client
        .snapshot(&root.join("nested"))
        .expect("snapshot")
        .expect("repository");
    let reported = snapshot.repository_root.expect("snapshot root");
    assert!(
        !reported.to_string_lossy().starts_with(r"\\?\"),
        "{reported:?}"
    );
    assert!(paths_equivalent(&reported, &expected), "{reported:?}");
}

/// Every program a repository can name in its own configuration and attributes only
/// touches a marker file. Inspecting the repository must create none of them, while the
/// reported state stays exact.
#[test]
fn hostile_repository_configuration_never_runs_during_inspection() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path().join("repository");
    let markers = directory.path().join("markers");
    fs::create_dir_all(&markers).expect("markers");
    init_repository(&root);

    fs::write(
        root.join(".gitattributes"),
        "*.clean filter=evilclean\n*.process filter=evilprocess\n*.txt diff=evildiff\n",
    )
    .expect("attributes");
    fs::create_dir_all(root.join(".git").join("info")).expect("info directory");
    fs::write(
        root.join(".git").join("info").join("attributes"),
        "*.info filter=evilinfo\n",
    )
    .expect("info attributes");
    for (name, content) in [
        ("staged.txt", "staged\n"),
        ("unstaged.txt", "unstaged\n"),
        ("filtered.clean", "clean\n"),
        ("filtered.process", "process\n"),
        ("filtered.info", "info\n"),
    ] {
        fs::write(root.join(name), content).expect("seed file");
    }
    git(&root, &["add", "."]);
    commit(&root, "initial");

    // HEAD carries a signature, so `log.showSignature` would run gpg.program on it.
    let tree = git_output(&root, &["rev-parse", "HEAD^{tree}"], None);
    let parent = git_output(&root, &["rev-parse", "HEAD"], None);
    let signed = git_output(
        &root,
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        Some(&format!(
            "tree {tree}\nparent {parent}\n\
             author ContextWake Tests <tests@example.invalid> 1700000000 +0000\n\
             committer ContextWake Tests <tests@example.invalid> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n =AAAA\n \
             -----END PGP SIGNATURE-----\n\nsigned subject\n"
        )),
    );
    git(&root, &["update-ref", "refs/heads/main", &signed]);

    fs::write(root.join("staged.txt"), "staged\nmore\n").expect("staged change");
    git(&root, &["add", "staged.txt"]);
    fs::write(root.join("unstaged.txt"), "unstaged\nmore\n").expect("unstaged change");
    fs::write(root.join("untracked.txt"), "untracked\n").expect("untracked file");
    for name in ["filtered.clean", "filtered.process", "filtered.info"] {
        backdate(&root.join(name));
    }

    // `#` turns any arguments Git appends into a shell comment.
    let marker = |name: &str| shell_path(&markers.join(name));
    let hooks = directory.path().join("attacker-hooks");
    fs::create_dir_all(&hooks).expect("attacker hooks");
    for hooks_directory in [root.join(".git").join("hooks"), hooks.clone()] {
        write_script(
            &hooks_directory.join("post-index-change"),
            &format!("touch '{}'", marker("hook")),
        );
    }
    let gpg = directory.path().join("fake-gpg");
    write_script(&gpg, &format!("touch '{}'\nexit 1", marker("gpg")));
    append(
        &root.join(".git").join("config"),
        &format!(
            "[core]\n\
             \tfsmonitor = \"touch '{fsmonitor}'; false #\"\n\
             \thooksPath = \"{hooks}\"\n\
             \tpager = \"touch '{pager}'; cat #\"\n\
             [filter \"evilclean\"]\n\
             \tclean = \"touch '{clean}'; cat #\"\n\
             \tsmudge = \"touch '{smudge}'; cat #\"\n\
             [filter \"evilprocess\"]\n\
             \tprocess = \"touch '{process}' #\"\n\
             [filter \"evilinfo\"]\n\
             \tclean = \"touch '{info}'; cat #\"\n\
             [diff]\n\
             \texternal = \"touch '{external}' #\"\n\
             [diff \"evildiff\"]\n\
             \ttextconv = \"touch '{textconv}'; cat #\"\n\
             \tcommand = \"touch '{command}' #\"\n\
             [log]\n\
             \tshowSignature = true\n\
             [gpg]\n\
             \tprogram = \"{gpg}\"\n\
             [hook \"audit\"]\n\
             \tcommand = \"touch '{configured}' #\"\n\
             \tevent = post-index-change\n\
             [pager]\n\
             \tstatus = true\n",
            fsmonitor = marker("fsmonitor"),
            hooks = shell_path(&hooks),
            pager = marker("pager"),
            clean = marker("clean"),
            smudge = marker("smudge"),
            process = marker("process"),
            info = marker("info-attributes"),
            external = marker("external-diff"),
            textconv = marker("textconv"),
            command = marker("diff-command"),
            gpg = shell_path(&gpg),
            configured = marker("configured-hook"),
        ),
    );

    let client = GitClient::new(Duration::from_secs(20));
    let found = client
        .repository_root(&root)
        .expect("root")
        .expect("repository");
    let snapshot = client
        .snapshot(&root)
        .expect("snapshot")
        .expect("repository");
    let files = client.changed_files(&root).expect("changed files");
    client.version().expect("Git version");

    assert_eq!(
        file_names(&markers),
        Vec::<String>::new(),
        "repository-controlled programs ran"
    );
    assert!(paths_equivalent(
        &found,
        &canonicalize_simplified(&root).expect("canonical root")
    ));
    assert!(!snapshot.timed_out);
    assert!(snapshot.dirty);
    assert_eq!(snapshot.branch.as_deref(), Some("main"));
    assert_eq!(snapshot.head_commit.as_deref(), Some(signed.as_str()));
    assert_eq!(snapshot.last_commit.as_deref(), Some("signed subject"));
    assert_eq!(
        (
            snapshot.staged_count,
            snapshot.unstaged_count,
            snapshot.untracked_count
        ),
        (1, 1, 1)
    );
    assert_eq!((snapshot.additions, snapshot.deletions), (2, 0));
    assert_eq!(
        files,
        ["staged.txt", "unstaged.txt", "untracked.txt"].map(PathBuf::from)
    );
}

#[test]
fn submodules_are_inspected_without_running_git_inside_them() {
    let directory = tempfile::tempdir().expect("tempdir");
    let markers = directory.path().join("markers");
    fs::create_dir_all(&markers).expect("markers");
    let library = directory.path().join("library");
    init_repository(&library);
    fs::write(library.join("lib.txt"), "library\n").expect("library file");
    git(&library, &["add", "lib.txt"]);
    commit(&library, "library");

    let root = directory.path().join("superproject");
    init_repository(&root);
    fs::write(root.join("README.md"), "superproject\n").expect("readme");
    git(&root, &["add", "README.md"]);
    commit(&root, "superproject");
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &shell_path(&library),
            "library",
        ],
    );
    commit(&root, "add library");

    // The submodule moves to a new commit (a real, uncommitted gitlink change) and has
    // dirty content; its own configuration names an fsmonitor command.
    let checkout = root.join("library");
    fs::write(checkout.join("more.txt"), "more\n").expect("submodule change");
    git(&checkout, &["add", "more.txt"]);
    commit(&checkout, "library update");
    fs::write(checkout.join("scratch.txt"), "scratch\n").expect("submodule scratch");
    append(
        &root
            .join(".git")
            .join("modules")
            .join("library")
            .join("config"),
        &format!(
            "[core]\n\tfsmonitor = \"touch '{}'; false #\"\n",
            shell_path(&markers.join("submodule-fsmonitor"))
        ),
    );

    let client = GitClient::new(Duration::from_secs(20));
    let snapshot = client
        .snapshot(&root)
        .expect("snapshot")
        .expect("repository");
    let files = client.changed_files(&root).expect("changed files");
    // Inside the checkout, the submodule's `.git` file (a relative `gitdir:` link) makes it
    // a work tree of its own.
    let submodule_root = client
        .repository_root(&checkout)
        .expect("root")
        .expect("submodule work tree");

    assert_eq!(
        file_names(&markers),
        Vec::<String>::new(),
        "Git ran inside the submodule"
    );
    assert!(paths_equivalent(
        &submodule_root,
        &canonicalize_simplified(&checkout).expect("canonical checkout")
    ));
    assert_eq!(
        (
            snapshot.staged_count,
            snapshot.unstaged_count,
            snapshot.untracked_count
        ),
        (0, 1, 0)
    );
    assert!(snapshot.dirty);
    assert_eq!(files, [PathBuf::from("library")]);
}

#[test]
fn linked_worktrees_are_inspected_as_their_own_work_tree() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path().join("main");
    init_repository(&root);
    fs::write(root.join("notes.txt"), "notes\n").expect("notes");
    git(&root, &["add", "notes.txt"]);
    commit(&root, "initial");
    let linked = directory.path().join("linked");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            &shell_path(&linked),
        ],
    );
    fs::write(linked.join("notes.txt"), "notes\nfeature\n").expect("linked change");

    let client = GitClient::new(Duration::from_secs(20));
    let found = client
        .repository_root(&linked)
        .expect("root")
        .expect("linked work tree");
    assert!(paths_equivalent(
        &found,
        &canonicalize_simplified(&linked).expect("canonical linked")
    ));
    let snapshot = client
        .snapshot(&linked)
        .expect("snapshot")
        .expect("linked work tree");
    assert_eq!(snapshot.branch.as_deref(), Some("feature"));
    assert_eq!(snapshot.unstaged_count, 1);
    assert_eq!(
        client.changed_files(&linked).expect("files"),
        [PathBuf::from("notes.txt")]
    );
}

/// A tracked file named `HEAD` makes `git diff HEAD` ambiguous unless revisions and
/// paths are separated; the diff summary then silently read `+0 -0`.
#[test]
fn diff_summary_survives_a_file_named_head() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path().join("repository");
    init_repository(&root);
    fs::write(root.join("HEAD"), "not a revision\n").expect("file named HEAD");
    fs::write(root.join("notes.txt"), "a\n").expect("notes");
    git(&root, &["add", "HEAD", "notes.txt"]);
    commit(&root, "initial");
    fs::write(root.join("notes.txt"), "a\nb\n").expect("modify notes");

    let snapshot = GitClient::new(Duration::from_secs(20))
        .snapshot(&root)
        .expect("snapshot")
        .expect("repository");
    assert_eq!(snapshot.unstaged_count, 1);
    assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
}

/// `core.worktree` in an untrusted repository must not make `ContextWake` register or
/// summarize another directory, such as the user's home directory.
#[test]
fn work_tree_redirect_is_not_inspected() {
    let directory = tempfile::tempdir().expect("tempdir");
    let elsewhere = directory.path().join("home");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    fs::write(elsewhere.join("private-notes.txt"), "private\n").expect("private file");
    let root = directory.path().join("repository");
    init_repository(&root);
    git(&root, &["config", "core.worktree", &shell_path(&elsewhere)]);

    let client = GitClient::new(Duration::from_secs(10));
    assert_eq!(client.repository_root(&root).expect("root"), None);
    assert_eq!(client.snapshot(&root).expect("snapshot"), None);
    assert!(client.changed_files(&root).expect("files").is_empty());
}

#[test]
fn inherited_git_environment_cannot_redirect_or_reconfigure_inspection() {
    let directory = tempfile::tempdir().expect("tempdir");
    let markers = directory.path().join("markers");
    fs::create_dir_all(&markers).expect("markers");
    let root = directory.path().join("repository");
    init_repository(&root);
    fs::write(root.join("tracked.txt"), "tracked\n").expect("tracked file");
    git(&root, &["add", "tracked.txt"]);
    commit(&root, "initial");
    fs::write(root.join("untracked.txt"), "untracked\n").expect("untracked file");
    let other = directory.path().join("other");
    init_repository(&other);

    let marker = |name: &str| shell_path(&markers.join(name));
    let mut command = ctx(&directory.path().join("home"));
    command
        .current_dir(&root)
        .env("GIT_DIR", other.join(".git"))
        .env("GIT_WORK_TREE", &other)
        .env("GIT_INDEX_FILE", directory.path().join("missing-index"))
        .env(
            "GIT_CONFIG_PARAMETERS",
            format!(
                "'core.fsmonitor=touch \"{}\"; false #'",
                marker("parameters")
            ),
        )
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("touch '{}'; false #", marker("count")),
        )
        .env("GIT_TRACE2_EVENT", marker("trace2"));
    let status = json_status(&mut command, &root);

    assert_eq!(file_names(&markers), Vec::<String>::new());
    let reported = status["git"]["repository_root"]
        .as_str()
        .expect("repository root");
    assert!(
        paths_equivalent(
            Path::new(reported),
            &canonicalize_simplified(&root).expect("canonical root")
        ),
        "{reported}"
    );
    assert_eq!(status["git"]["untracked_count"], 1);
    assert_eq!(status["git"]["staged_count"], 0);
}

#[test]
fn missing_git_leaves_ordinary_directories_usable() {
    let directory = tempfile::tempdir().expect("tempdir");
    let workspace = directory.path().join("notes");
    fs::create_dir_all(&workspace).expect("workspace");

    let client =
        GitClient::with_executable("contextwake-test-missing-git", Duration::from_millis(200));
    assert!(!client.is_available());
    assert_eq!(client.repository_root(&workspace).expect("root"), None);
    assert_eq!(client.snapshot(&workspace).expect("snapshot"), None);
    assert!(client.changed_files(&workspace).expect("files").is_empty());
    let error = client.version().expect_err("Git is missing").to_string();
    assert!(error.contains("not found"), "{error}");
    assert!(!error.contains("configure its executable path"), "{error}");

    // `ctx status` with no Git anywhere on PATH still reports the ordinary directory.
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let without_git = std::env::split_paths(&search_path)
        .filter(|entry| {
            !["git", "git.exe"]
                .iter()
                .any(|name| entry.join(name).is_file())
        })
        .collect::<Vec<_>>();
    let mut command = ctx(&directory.path().join("home"));
    command.env(
        "PATH",
        std::env::join_paths(without_git).expect("PATH without Git"),
    );
    let status = json_status(&mut command, &workspace);
    assert!(status["git"].is_null(), "{status}");
    assert!(
        paths_equivalent(
            Path::new(
                status["workspace"]["path"]
                    .as_str()
                    .expect("workspace path")
            ),
            &canonicalize_simplified(&workspace).expect("canonical workspace")
        ),
        "{status}"
    );
}
