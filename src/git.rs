//! Bounded, hardened Git inspection.
//!
//! `ContextWake` inspects Git on its own: at TUI start, in `ctx status` and whenever it
//! creates a checkpoint, including in directories nobody has trusted. A repository's own
//! configuration (`.git/config`, files it includes, worktree configuration) together with
//! its attributes can name programs that Git runs while it merely reads the index or the
//! work tree: an fsmonitor command, filter drivers, diff drivers, hooks (in a hooks
//! directory or configured as `hook.<name>.command`), signature verifiers and pagers.
//! Every invocation here therefore:
//!
//! * starts an absolute Git executable found through absolute `PATH` entries (never a
//!   Windows batch file), with standard input closed;
//! * drops inherited `GIT_*` variables that select another repository or index, inject
//!   configuration, name programs or write traces, and disables prompts, optional locks
//!   and lazy fetches;
//! * overrides on the command line, which outranks every configuration file, each setting
//!   that runs a program; filter drivers and configured hooks are disabled by name after a
//!   non-executing `git config` listing;
//! * never writes the index, never runs Git inside submodules and never contacts a remote;
//! * ignores a work tree that does not contain the repository Git found (a `core.worktree`
//!   redirect);
//! * bounds every wait: on timeout the whole process tree is stopped, and pipes that a
//!   stray descendant keeps open are abandoned after a short grace period.
//!
//! Without a usable Git executable every directory is an ordinary, non-Git workspace. A
//! `git status` that does not finish is reported as dirty with `timed_out` set, never as
//! clean.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::model::GitSnapshot;
use crate::paths::{canonicalize_simplified, paths_equivalent, simplify_path};
use crate::security::sanitize_terminal;

/// Most standard output kept from one Git command. `git status` output for several
/// hundred thousand paths fits; larger output is treated as incomplete.
const MAX_STDOUT_BYTES: usize = 64 * 1024 * 1024;
/// Most diagnostic output kept from one Git command.
const MAX_STDERR_BYTES: usize = 64 * 1024;
/// How long output pipes may stay open after Git exited or was stopped.
const PIPE_GRACE: Duration = Duration::from_secs(2);
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_secs(5);
/// Most filter drivers and configured hooks one repository may declare before
/// `ContextWake` refuses to inspect it (each one costs command-line space).
const MAX_NEUTRALIZED_NAMES: usize = 128;
/// Most bytes the generated `-c` settings may take. Windows limits a whole command line
/// to 32,767 characters, so overly long names are refused instead of failing the spawn.
const MAX_NEUTRALIZED_BYTES: usize = 16 * 1024;
/// Appended to the Git executable path to form `core.hooksPath`. The executable is a
/// regular file, so nothing can exist below it and no directory has to be created.
const DISABLED_HOOKS_DIRECTORY: &str = "contextwake-hooks-disabled";

/// `-c` settings passed to every invocation. Command-line settings outrank every
/// configuration file, including the repository's own.
const HARDENED_SETTINGS: &[&str] = &[
    // Never run an fsmonitor hook or start the fsmonitor daemon.
    "core.fsmonitor=false",
    // ContextWake never writes the index, so it has no use for the untracked cache.
    "core.untrackedCache=false",
    // Porcelain `git diff` rewrites the index (firing post-index-change) after stat-only
    // changes, even with optional locks disabled.
    "diff.autoRefreshIndex=false",
    "diff.external=",
    "core.pager=cat",
    "log.showSignature=false",
    // Embedded bare repositories are used only when named explicitly.
    "safe.bareRepository=explicit",
    // No transport at all: partial-clone lazy fetches would run repository-chosen ssh
    // commands and credential helpers.
    "protocol.allow=never",
    // Hooks configured as `hook.<name>.event` are not looked up in core.hooksPath.
    "hook.post-index-change.enabled=false",
    "i18n.logOutputEncoding=UTF-8",
];

const DISCOVER: &[&str] = &["rev-parse", "--show-toplevel", "--absolute-git-dir"];
const LIST_PROGRAM_SETTINGS: &[&str] = &[
    "config",
    "-z",
    "--name-only",
    "--get-regexp",
    r"^(filter|hook)\.",
];
const STATUS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "-z",
    "--branch",
    "--untracked-files=all",
    // Report submodule commit changes without running Git inside the submodule.
    "--ignore-submodules=dirty",
];
const NUMSTAT: &[&str] = &[
    "diff",
    "--numstat",
    "--no-ext-diff",
    "--no-textconv",
    "--ignore-submodules=dirty",
    "HEAD",
    "--",
];
const LAST_COMMIT: &[&str] = &["log", "-1", "--no-show-signature", "--format=%H%x09%s"];
const BRANCH: &[&str] = &["symbolic-ref", "--quiet", "--short", "HEAD"];

/// Inherited variables that would point Git at another repository or index, inject
/// configuration, or name programs to run.
const SCRUBBED_VARIABLES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIFF_OPTS",
    "GIT_PAGER",
    "GIT_PAGER_IN_USE",
    "PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "GIT_ALLOW_PROTOCOL",
    "GIT_PROTOCOL_FROM_USER",
];
/// Variable families removed the same way (`GIT_SSH_COMMAND`, `GIT_TRACE2_EVENT`, ...).
const SCRUBBED_PREFIXES: &[&str] = &[
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "GIT_SSH",
    "GIT_TRACE",
];

#[derive(Clone, Debug)]
pub struct GitClient {
    executable: PathBuf,
    timeout: Duration,
}

#[derive(Debug, Default)]
struct CommandOutput {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
    /// Stopped at the deadline, or its output stayed open after it exited.
    timed_out: bool,
    /// Exited on its own, and all of its output was captured.
    complete: bool,
}

impl CommandOutput {
    fn success(&self) -> bool {
        self.complete && self.code == Some(0)
    }
}

type Runner = dyn Fn(Command, Duration) -> std::io::Result<CommandOutput>;

/// One resolved Git executable plus the way its commands are run.
struct Session<'r> {
    executable: PathBuf,
    timeout: Duration,
    runner: &'r Runner,
}

#[derive(Debug, Default)]
struct StatusReport {
    snapshot: GitSnapshot,
    paths: BTreeSet<PathBuf>,
}

impl GitClient {
    pub fn new(timeout: Duration) -> Self {
        Self {
            executable: PathBuf::from("git"),
            timeout,
        }
    }

    pub fn with_executable(executable: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            executable: executable.into(),
            timeout,
        }
    }

    /// Whether a Git executable that `ContextWake` is willing to run can be found. Nothing
    /// is started. Without one, every directory is treated as an ordinary workspace.
    pub fn is_available(&self) -> bool {
        self.session().is_ok()
    }

    /// Why no Git executable can be used, or `None` when one can. Nothing is started.
    pub fn unavailable_reason(&self) -> Option<String> {
        self.session().err()
    }

    pub fn version(&self) -> Result<String> {
        let session = self.session().map_err(ContextWakeError::Git)?;
        let output = session.run(None, &[], &["--version"])?;
        if output.timed_out {
            return Err(ContextWakeError::Git(format!(
                "Git did not report its version within {} ms",
                self.timeout.as_millis()
            )));
        }
        if !output.success() {
            return Err(ContextWakeError::Git(actionable_git_error(&output.stderr)));
        }
        Ok(sanitize_terminal(
            String::from_utf8_lossy(&output.stdout).trim(),
        ))
    }

    /// The top-level directory, in conventional form, of the Git work tree that contains
    /// `path`. `Ok(None)` for an ordinary directory, when Git is unavailable, and when Git
    /// does not answer in time.
    pub fn repository_root(&self, path: &Path) -> Result<Option<PathBuf>> {
        let directory = inspection_directory(path)?;
        Ok(self
            .session()
            .ok()
            .and_then(|session| session.discover(&directory)))
    }

    /// Branch, HEAD and change counts of the work tree containing `path`. `Ok(None)` for
    /// an ordinary directory or when Git is unavailable. When `git status` cannot finish,
    /// the snapshot has `timed_out` set and is marked dirty: its state is unknown.
    pub fn snapshot(&self, path: &Path) -> Result<Option<GitSnapshot>> {
        let directory = inspection_directory(path)?;
        let Ok(session) = self.session() else {
            return Ok(None);
        };
        let Some(root) = session.discover(&directory) else {
            return Ok(None);
        };
        session.snapshot(&root).map(Some)
    }

    /// Staged, unstaged and untracked paths relative to the work tree root. Empty for an
    /// ordinary directory, when Git is unavailable, and when `git status` cannot finish
    /// (the matching snapshot then reports `timed_out`).
    pub fn changed_files(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let directory = inspection_directory(path)?;
        let Ok(session) = self.session() else {
            return Ok(Vec::new());
        };
        let Some(root) = session.discover(&directory) else {
            return Ok(Vec::new());
        };
        session.changed_files(&root)
    }

    fn session(&self) -> std::result::Result<Session<'static>, String> {
        let executable = resolve_executable(&self.executable, std::env::var_os("PATH").as_deref())?;
        Ok(Session {
            executable,
            timeout: self.timeout,
            runner: &run_bounded,
        })
    }
}

impl Session<'_> {
    fn command(&self, cwd: Option<&Path>, settings: &[OsString], args: &[&str]) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(["--no-pager", "--no-optional-locks"]);
        for setting in HARDENED_SETTINGS {
            command.arg("-c").arg(setting);
        }
        let mut hooks = OsString::from("core.hooksPath=");
        hooks.push(self.executable.join(DISABLED_HOOKS_DIRECTORY));
        command.arg("-c").arg(hooks);
        for setting in settings {
            command.arg("-c").arg(setting);
        }
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        scrub_environment(&mut command);
        command
    }

    fn execute(
        &self,
        cwd: Option<&Path>,
        settings: &[OsString],
        args: &[&str],
    ) -> std::io::Result<CommandOutput> {
        (self.runner)(self.command(cwd, settings, args), self.timeout)
    }

    fn run(
        &self,
        cwd: Option<&Path>,
        settings: &[OsString],
        args: &[&str],
    ) -> Result<CommandOutput> {
        self.execute(cwd, settings, args).map_err(|error| {
            ContextWakeError::Git(format!(
                "could not run {}: {error}",
                sanitize_terminal(&self.executable.display().to_string())
            ))
        })
    }

    /// The conventional work tree root for `directory`, or `None` when it is not inside a
    /// usable work tree (including when Git cannot be started or does not answer).
    fn discover(&self, directory: &Path) -> Option<PathBuf> {
        let output = self.execute(Some(directory), &[], DISCOVER).ok()?;
        if !output.success() {
            return None;
        }
        let mut lines = output
            .stdout
            .split(|byte| *byte == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
        let root = conventional_path(&path_from_output(lines.next()?)?);
        let git_dir = conventional_path(&path_from_output(lines.next()?)?);
        work_tree_owns(&root, &git_dir).then_some(root)
    }

    /// `-c` settings that disable every filter driver and configured hook visible to the
    /// repository, from any configuration scope. `Ok(None)` when Git did not answer.
    ///
    /// User-level drivers such as Git LFS are disabled as well: a repository chooses, via
    /// its attributes, which driver runs on which file. A stat-dirty LFS file can then be
    /// reported as modified until Git itself refreshes the index.
    fn neutralizing_settings(&self, root: &Path) -> Result<Option<Vec<OsString>>> {
        let output = self.run(Some(root), &[], LIST_PROGRAM_SETTINGS)?;
        if !output.complete {
            return Ok(None);
        }
        match output.code {
            Some(0) => neutralizing_overrides(&output.stdout).map(Some),
            // `git config --get-regexp` exits with 1 when nothing matches.
            Some(1) => Ok(Some(Vec::new())),
            _ => Err(ContextWakeError::Git(actionable_git_error(&output.stderr))),
        }
    }

    fn snapshot(&self, root: &Path) -> Result<GitSnapshot> {
        let Some(settings) = self.neutralizing_settings(root)? else {
            return self.unknown_state(root, &[]);
        };
        let status = self.run(Some(root), &settings, STATUS)?;
        let Some(report) = interpret_status(&status)? else {
            return self.unknown_state(root, &settings);
        };
        let mut snapshot = report.snapshot;
        snapshot.repository_root = Some(root.to_path_buf());

        let diff = self.run(Some(root), &settings, NUMSTAT)?;
        if diff.success() {
            (snapshot.additions, snapshot.deletions) =
                parse_numstat(&String::from_utf8_lossy(&diff.stdout));
        }
        self.read_last_commit(root, &settings, &mut snapshot)?;
        Ok(snapshot)
    }

    fn changed_files(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let Some(settings) = self.neutralizing_settings(root)? else {
            return Ok(Vec::new());
        };
        let status = self.run(Some(root), &settings, STATUS)?;
        Ok(interpret_status(&status)?
            .map(|report| report.paths.into_iter().collect())
            .unwrap_or_default())
    }

    /// `git status` did not finish, so the work tree state is unknown and must not be
    /// reported as clean. Branch and HEAD still come from commands that read neither the
    /// index nor the work tree (so no filter or hook can run even without `settings`).
    fn unknown_state(&self, root: &Path, settings: &[OsString]) -> Result<GitSnapshot> {
        let mut snapshot = GitSnapshot {
            repository_root: Some(root.to_path_buf()),
            dirty: true,
            timed_out: true,
            ..GitSnapshot::default()
        };
        let branch = self.run(Some(root), settings, BRANCH)?;
        if branch.success() {
            snapshot.branch = Some(sanitize_terminal(
                String::from_utf8_lossy(&branch.stdout).trim(),
            ))
            .filter(|name| !name.is_empty());
        } else if branch.complete && branch.code == Some(1) {
            snapshot.detached_head = true;
        }
        self.read_last_commit(root, settings, &mut snapshot)?;
        Ok(snapshot)
    }

    fn read_last_commit(
        &self,
        root: &Path,
        settings: &[OsString],
        snapshot: &mut GitSnapshot,
    ) -> Result<()> {
        let commit = self.run(Some(root), settings, LAST_COMMIT)?;
        if commit.success() {
            let line = sanitize_terminal(String::from_utf8_lossy(&commit.stdout).trim());
            let mut parts = line.splitn(2, '\t');
            if let Some(head) = parts.next().filter(|head| !head.is_empty()) {
                snapshot.head_commit = Some(head.to_owned());
            }
            snapshot.last_commit = parts.next().map(ToOwned::to_owned);
        }
        Ok(())
    }
}

/// Resolves the Git executable without starting anything. A bare program name is looked
/// up in the absolute entries of `search_path` only: a relative entry would resolve
/// against the current directory, which may be an untrusted repository.
fn resolve_executable(
    executable: &Path,
    search_path: Option<&OsStr>,
) -> std::result::Result<PathBuf, String> {
    let shown = sanitize_terminal(&executable.display().to_string());
    let file_name = executable_file_name(executable, &shown)?;
    if executable.is_absolute() {
        let candidate = executable.with_file_name(&file_name);
        return if is_runnable_file(&candidate) {
            Ok(candidate)
        } else {
            Err(format!("Git executable not found: {shown}"))
        };
    }
    let mut components = executable.components();
    if !matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return Err(format!(
            "the Git executable must be a program name or an absolute path: {shown}"
        ));
    }
    search_directories(search_path)
        .into_iter()
        .map(|directory| directory.join(&file_name))
        .find(|candidate| is_runnable_file(candidate))
        .ok_or_else(|| format!("{shown} was not found on PATH"))
}

/// The absolute directories of a `PATH`-style value, in order.
fn search_directories(search_path: Option<&OsStr>) -> Vec<PathBuf> {
    search_path
        .map(|value| {
            std::env::split_paths(value)
                .filter(|directory| directory.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// The file name to run. On Windows only `.exe` files are accepted: a `.cmd` or `.bat`
/// wrapper would hand repository-derived arguments (filter and hook names) to cmd.exe.
fn executable_file_name(executable: &Path, shown: &str) -> std::result::Result<OsString, String> {
    let name = executable
        .file_name()
        .ok_or_else(|| format!("the Git executable has no file name: {shown}"))?;
    if !cfg!(windows) {
        return Ok(name.to_os_string());
    }
    match executable.extension() {
        None => {
            let mut name = name.to_os_string();
            name.push(".exe");
            Ok(name)
        }
        Some(extension) if extension.eq_ignore_ascii_case("exe") => Ok(name.to_os_string()),
        Some(_) => Err(format!(
            "refusing to run {shown}: only .exe Git executables are supported on Windows"
        )),
    }
}

fn is_runnable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The directory Git is started in: an existing directory, in conventional form.
fn inspection_directory(path: &Path) -> Result<PathBuf> {
    let directory = simplify_path(path);
    if std::fs::metadata(&directory).at(&directory)?.is_dir() {
        Ok(directory)
    } else {
        Err(ContextWakeError::UnsafePath(format!(
            "{} is not a directory",
            sanitize_terminal(&directory.display().to_string())
        )))
    }
}

fn conventional_path(path: &Path) -> PathBuf {
    canonicalize_simplified(path).unwrap_or_else(|_| simplify_path(path))
}

fn path_from_output(bytes: &[u8]) -> Option<PathBuf> {
    if bytes.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

/// True when `root/.git` is, or points to, `git_dir`. A `core.worktree` setting could
/// otherwise make Git scan, and `ContextWake` register and summarize, any other directory,
/// such as the user's home directory. Linked worktrees and submodules use a `.git` file
/// that names their Git directory.
fn work_tree_owns(root: &Path, git_dir: &Path) -> bool {
    let entry = root.join(".git");
    let Ok(metadata) = std::fs::metadata(&entry) else {
        return false;
    };
    let target = if metadata.is_dir() {
        entry
    } else if metadata.is_file() {
        let Some(target) = gitfile_target(&entry, root) else {
            return false;
        };
        target
    } else {
        return false;
    };
    paths_equivalent(&conventional_path(&target), git_dir)
}

fn gitfile_target(entry: &Path, root: &Path) -> Option<PathBuf> {
    let mut content = String::new();
    std::fs::File::open(entry)
        .ok()?
        .take(4_096)
        .read_to_string(&mut content)
        .ok()?;
    let target = content.lines().next()?.strip_prefix("gitdir: ")?;
    (!target.is_empty()).then(|| root.join(target))
}

/// Builds the `-c` settings that disable the filter drivers and configured hooks named in
/// a `git config -z --name-only` listing.
fn neutralizing_overrides(listing: &[u8]) -> Result<Vec<OsString>> {
    let refuse = |reason: &str| {
        ContextWakeError::UntrustedConfiguration(format!(
            "Git configuration declares {reason}; ContextWake did not inspect the repository"
        ))
    };
    let listing = std::str::from_utf8(listing)
        .map_err(|_| refuse("filter drivers or hooks whose names are not valid UTF-8"))?;
    let mut filters = BTreeSet::new();
    let mut hooks = BTreeSet::new();
    for key in listing.split('\0').filter(|key| !key.is_empty()) {
        // `filter.<name>.clean`: the name sits between the first and the last dot and may
        // itself contain dots. `hook.jobs` has no name and is ignored.
        let Some((qualified, _variable)) = key.rsplit_once('.') else {
            continue;
        };
        if let Some(name) = qualified.strip_prefix("filter.") {
            filters.insert(name);
        } else if let Some(name) = qualified.strip_prefix("hook.") {
            hooks.insert(name);
        }
    }
    if filters.len() + hooks.len() > MAX_NEUTRALIZED_NAMES {
        return Err(refuse(
            "more filter drivers and hooks than ContextWake can disable",
        ));
    }
    // `-c name=value` splits at the first '=', so such a name cannot be overridden.
    if filters
        .iter()
        .chain(&hooks)
        .any(|name| name.contains(['=', '\n']))
    {
        return Err(refuse(
            "a filter driver or hook name that cannot be disabled",
        ));
    }
    let mut settings = Vec::with_capacity(filters.len() * 4 + hooks.len());
    for name in filters {
        settings.extend(
            [
                format!("filter.{name}.clean="),
                format!("filter.{name}.smudge="),
                format!("filter.{name}.process="),
                format!("filter.{name}.required=false"),
            ]
            .map(OsString::from),
        );
    }
    for name in hooks {
        settings.push(OsString::from(format!("hook.{name}.enabled=false")));
    }
    // Each setting also costs a `-c` argument and separators on the command line.
    let bytes: usize = settings.iter().map(|setting| setting.len() + 4).sum();
    if bytes > MAX_NEUTRALIZED_BYTES {
        return Err(refuse(
            "filter driver or hook names too long for ContextWake to disable",
        ));
    }
    Ok(settings)
}

fn scrub_environment(command: &mut Command) {
    for name in SCRUBBED_VARIABLES {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if is_scrubbed_variable(&name) {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1");
}

fn is_scrubbed_variable(name: &OsStr) -> bool {
    let mut name = name.as_encoded_bytes().to_vec();
    if cfg!(windows) {
        // Windows environment variable names are case-insensitive.
        name.make_ascii_uppercase();
    }
    SCRUBBED_VARIABLES
        .iter()
        .any(|variable| name == variable.as_bytes())
        || SCRUBBED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix.as_bytes()))
}

/// Interprets one `git status` run: `Ok(None)` when it did not finish or its output is
/// incomplete, an error when Git reported a failure.
fn interpret_status(status: &CommandOutput) -> Result<Option<StatusReport>> {
    if !status.complete {
        return Ok(None);
    }
    if status.code != Some(0) {
        return Err(ContextWakeError::Git(actionable_git_error(&status.stderr)));
    }
    Ok(Some(parse_status(&status.stdout)))
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
    failed: bool,
}

/// Runs `command` with closed standard input and captured output, waiting at most
/// `timeout` for it and at most [`PIPE_GRACE`] for its output pipes afterwards.
fn run_bounded(command: Command, timeout: Duration) -> std::io::Result<CommandOutput> {
    run_with_pipe_grace(command, timeout, PIPE_GRACE)
}

fn run_with_pipe_grace(
    mut command: Command,
    timeout: Duration,
    pipe_grace: Duration,
) -> std::io::Result<CommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let (sender, receiver) = mpsc::channel();
    let readers = match (child.stdout.take(), child.stderr.take()) {
        (Some(stdout), Some(stderr)) => {
            spawn_reader(stdout, Stream::Stdout, MAX_STDOUT_BYTES, sender.clone())
                .and_then(|()| spawn_reader(stderr, Stream::Stderr, MAX_STDERR_BYTES, sender))
        }
        _ => Err(std::io::Error::other("Git output could not be captured")),
    };
    if let Err(error) = readers {
        stop_process_tree(&mut child);
        return Err(error);
    }

    let (code, timed_out) = match child.wait_timeout(timeout) {
        Ok(Some(status)) => (status.code(), false),
        Ok(None) => {
            stop_process_tree(&mut child);
            (None, true)
        }
        Err(error) => {
            stop_process_tree(&mut child);
            return Err(error);
        }
    };

    // The pipes close as soon as Git is gone, unless a descendant that inherited them is
    // still running. Wait for that only briefly; an abandoned reader thread ends when the
    // last writer closes its pipe.
    let deadline = Instant::now() + pipe_grace;
    let mut stdout = None;
    let mut stderr = None;
    while stdout.is_none() || stderr.is_none() {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((Stream::Stdout, capture)) => stdout = Some(capture),
            Ok((Stream::Stderr, capture)) => stderr = Some(capture),
            Err(_) => break,
        }
    }
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Ok(CommandOutput {
            code,
            timed_out: true,
            ..CommandOutput::default()
        });
    };
    let complete = !timed_out && !stdout.truncated && !stdout.failed && !stderr.failed;
    Ok(CommandOutput {
        code,
        stdout: stdout.bytes,
        stderr: sanitize_terminal(&String::from_utf8_lossy(&stderr.bytes)),
        timed_out,
        complete,
    })
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    which: Stream,
    limit: usize,
    sender: mpsc::Sender<(Stream, Capture)>,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("contextwake-git-output".into())
        .spawn(move || {
            let _ = sender.send((which, read_limited(stream, limit)));
        })
        .map(drop)
}

/// Reads `stream` to its end, keeping at most `limit` bytes.
fn read_limited(mut stream: impl Read, limit: usize) -> Capture {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Capture {
                    bytes,
                    truncated,
                    failed: false,
                };
            }
            Ok(read) => {
                let room = limit.saturating_sub(bytes.len());
                truncated |= read > room;
                bytes.extend_from_slice(&chunk[..read.min(room)]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Capture {
                    bytes,
                    truncated,
                    failed: true,
                };
            }
        }
    }
}

/// Stops `child` and, on Windows, everything it started, then reaps it.
fn stop_process_tree(child: &mut Child) {
    #[cfg(windows)]
    stop_descendants(child.id());
    let _ = child.kill();
    let _ = child.wait_timeout(PIPE_GRACE);
}

/// Git for Windows' `cmd\git.exe` is a launcher: killing it alone leaves the real Git, and
/// anything that Git started, running with our pipes open. `taskkill /T` walks the tree
/// from the still-running root (its process ID cannot be reused while we hold its handle).
#[cfg(windows)]
fn stop_descendants(process_id: u32) {
    let Some(system_root) = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    else {
        return;
    };
    let Ok(mut taskkill) = Command::new(system_root.join("System32").join("taskkill.exe"))
        .args(["/PID", &process_id.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if !matches!(taskkill.wait_timeout(TASKKILL_TIMEOUT), Ok(Some(_))) {
        let _ = taskkill.kill();
        let _ = taskkill.wait_timeout(PIPE_GRACE);
    }
}

fn actionable_git_error(stderr: &str) -> String {
    let message = sanitize_terminal(stderr.trim());
    if message.is_empty() {
        "Git returned a non-zero status. Run `ctx doctor` for details.".into()
    } else {
        message
    }
}

/// Parses `git status --porcelain=v2 -z --branch` output.
fn parse_status(output: &[u8]) -> StatusReport {
    let output = String::from_utf8_lossy(output);
    let mut report = StatusReport::default();
    let snapshot = &mut report.snapshot;
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            parse_branch_header(snapshot, &sanitize_terminal(header));
            continue;
        }
        let Some((kind, fields)) = record.split_once(' ') else {
            continue;
        };
        let path = match kind {
            // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            "1" => {
                count_change(snapshot, fields);
                fields.splitn(8, ' ').nth(7)
            }
            // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>, then <origPath>
            "2" => {
                count_change(snapshot, fields);
                let path = fields.splitn(9, ' ').nth(8);
                records.next();
                path
            }
            // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            "u" => {
                snapshot.staged_count += 1;
                snapshot.unstaged_count += 1;
                fields.splitn(10, ' ').nth(9)
            }
            "?" => {
                snapshot.untracked_count += 1;
                Some(fields)
            }
            _ => None,
        };
        if let Some(path) = path.map(|path| PathBuf::from(sanitize_terminal(path)))
            && is_work_tree_relative(&path)
        {
            report.paths.insert(path);
        }
    }
    snapshot.dirty =
        snapshot.staged_count > 0 || snapshot.unstaged_count > 0 || snapshot.untracked_count > 0;
    report
}

fn parse_branch_header(snapshot: &mut GitSnapshot, header: &str) {
    if let Some(value) = header.strip_prefix("branch.head ") {
        if value == "(detached)" {
            snapshot.detached_head = true;
        } else {
            snapshot.branch = Some(value.to_string());
        }
    } else if let Some(value) = header.strip_prefix("branch.oid ") {
        if value != "(initial)" {
            snapshot.head_commit = Some(value.to_string());
        }
    } else if let Some(value) = header.strip_prefix("branch.ab ") {
        for part in value.split_whitespace() {
            if let Some(ahead) = part.strip_prefix('+') {
                snapshot.ahead = ahead.parse().ok();
            } else if let Some(behind) = part.strip_prefix('-') {
                snapshot.behind = behind.parse().ok();
            }
        }
    }
}

fn count_change(snapshot: &mut GitSnapshot, fields: &str) {
    let mut xy = fields.chars();
    if xy.next().is_some_and(|index| index != '.') {
        snapshot.staged_count += 1;
    }
    if xy.next().is_some_and(|worktree| worktree != '.') {
        snapshot.unstaged_count += 1;
    }
}

fn is_work_tree_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && !path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}

fn parse_numstat(output: &str) -> (u64, u64) {
    output.lines().fold((0, 0), |(additions, deletions), line| {
        let mut fields = line.split('\t');
        let added = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let deleted = fields
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (additions + added, deletions + deleted)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_counts() {
        let report = parse_status(
            b"# branch.oid abc123\0# branch.head feature/test\0# branch.ab +2 -1\0\
              1 M. N... 100644 100644 100644 aaa bbb file\0\
              1 .M N... 100644 100644 100644 aaa bbb other\0\
              ? untracked\0",
        );
        let snapshot = report.snapshot;
        assert_eq!(snapshot.branch.as_deref(), Some("feature/test"));
        assert_eq!(snapshot.head_commit.as_deref(), Some("abc123"));
        assert_eq!(snapshot.ahead, Some(2));
        assert_eq!(snapshot.behind, Some(1));
        assert_eq!(snapshot.staged_count, 1);
        assert_eq!(snapshot.unstaged_count, 1);
        assert_eq!(snapshot.untracked_count, 1);
        assert!(snapshot.dirty);
        assert_eq!(
            report.paths.into_iter().collect::<Vec<_>>(),
            ["file", "other", "untracked"].map(PathBuf::from)
        );
    }

    #[test]
    fn parses_renames_conflicts_and_unsafe_paths() {
        let report = parse_status(
            b"# branch.oid (initial)\0# branch.head (detached)\0\
              2 R. N... 100644 100644 100644 aaa bbb R100 new name.rs\0old name.rs\0\
              u UU N... 100644 100644 100644 100644 aaa bbb ccc both.rs\0\
              ? ../escape\0? \x1b[31mred.txt\0! ignored\0",
        );
        let snapshot = report.snapshot;
        assert!(snapshot.detached_head);
        assert_eq!(snapshot.branch, None);
        assert_eq!(snapshot.head_commit, None);
        assert_eq!(snapshot.staged_count, 2);
        assert_eq!(snapshot.unstaged_count, 1);
        assert_eq!(snapshot.untracked_count, 2);
        assert_eq!(
            report.paths.into_iter().collect::<Vec<_>>(),
            ["both.rs", "new name.rs", "red.txt"].map(PathBuf::from)
        );
    }

    #[test]
    fn parses_text_and_binary_numstat() {
        assert_eq!(parse_numstat("12\t4\tsrc/a.rs\n-\t-\timage.png\n"), (12, 4));
    }

    #[test]
    fn every_listed_filter_driver_and_hook_is_disabled() {
        let settings = neutralizing_overrides(
            b"filter.lfs.clean\0filter.lfs.process\0filter.Evil.Dots.clean\0filter..clean\0\
              hook.audit.command\0hook.audit.event\0hook.jobs\0",
        )
        .expect("settings");
        let settings = settings
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for name in ["", "Evil.Dots", "lfs"] {
            for setting in [
                format!("filter.{name}.clean="),
                format!("filter.{name}.smudge="),
                format!("filter.{name}.process="),
                format!("filter.{name}.required=false"),
            ] {
                assert!(settings.contains(&setting), "{setting} missing");
            }
        }
        assert!(settings.contains(&"hook.audit.enabled=false".to_string()));
        assert_eq!(settings.len(), 13, "{settings:?}");
        assert!(neutralizing_overrides(b"").expect("empty").is_empty());
    }

    #[test]
    fn unrepresentable_or_excessive_program_settings_are_refused() {
        assert!(matches!(
            neutralizing_overrides(b"filter.a=b.clean\0"),
            Err(ContextWakeError::UntrustedConfiguration(_))
        ));
        assert!(matches!(
            neutralizing_overrides(b"hook.\xff.command\0"),
            Err(ContextWakeError::UntrustedConfiguration(_))
        ));
        let mut many = Vec::new();
        for index in 0..=MAX_NEUTRALIZED_NAMES {
            many.extend_from_slice(format!("filter.f{index}.clean\0").as_bytes());
        }
        assert!(matches!(
            neutralizing_overrides(&many),
            Err(ContextWakeError::UntrustedConfiguration(_))
        ));
        let last = format!("filter.f{MAX_NEUTRALIZED_NAMES}.clean\0");
        assert_eq!(
            neutralizing_overrides(&many[..many.len() - last.len()])
                .expect("exactly at the limit")
                .len(),
            MAX_NEUTRALIZED_NAMES * 4
        );
    }

    #[test]
    fn overly_long_program_settings_are_refused() {
        // 128 names of 60 characters stay within the name limit but would exceed the
        // Windows command-line limit once expanded into four settings each.
        let mut long = Vec::new();
        for index in 0..MAX_NEUTRALIZED_NAMES {
            long.extend_from_slice(format!("filter.{index:0>60}.clean\0").as_bytes());
        }
        assert!(matches!(
            neutralizing_overrides(&long),
            Err(ContextWakeError::UntrustedConfiguration(_))
        ));
        let name = "n".repeat(60);
        assert_eq!(
            neutralizing_overrides(format!("filter.{name}.clean\0").as_bytes())
                .expect("one long name fits")
                .len(),
            4
        );
    }

    #[test]
    fn repository_and_program_selecting_variables_are_scrubbed() {
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_12",
            "GIT_SSH_COMMAND",
            "GIT_TRACE2_EVENT",
            "GIT_EXTERNAL_DIFF",
            "PAGER",
        ] {
            assert!(is_scrubbed_variable(OsStr::new(name)), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "GIT_CEILING_DIRECTORIES",
            "GIT_CONFIG_GLOBAL",
            "XGIT_DIR",
        ] {
            assert!(!is_scrubbed_variable(OsStr::new(name)), "{name}");
        }
        #[cfg(windows)]
        assert!(is_scrubbed_variable(OsStr::new("git_dir")));

        let command = Session {
            executable: PathBuf::from("git"),
            timeout: Duration::from_secs(1),
            runner: &run_bounded,
        }
        .command(None, &[], &["status"]);
        let environment = command.get_envs().collect::<Vec<_>>();
        assert!(environment.contains(&(OsStr::new("GIT_DIR"), None)));
        assert!(environment.contains(&(OsStr::new("GIT_OPTIONAL_LOCKS"), Some(OsStr::new("0")))));
        assert!(environment.contains(&(OsStr::new("GIT_NO_LAZY_FETCH"), Some(OsStr::new("1")))));
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..2], ["--no-pager", "--no-optional-locks"]);
        assert!(args.contains(&"core.fsmonitor=false".to_string()));
        assert!(args.iter().any(
            |arg| arg.starts_with("core.hooksPath=") && arg.ends_with(DISABLED_HOOKS_DIRECTORY)
        ));
        assert_eq!(args.last().map(String::as_str), Some("status"));
    }

    #[test]
    fn relative_search_path_entries_are_ignored() {
        let absolute = tempfile::tempdir().expect("tempdir");
        let value = std::env::join_paths([
            PathBuf::from("relative-bin"),
            PathBuf::from("."),
            absolute.path().to_path_buf(),
        ])
        .expect("PATH value");
        assert_eq!(
            search_directories(Some(&value)),
            [absolute.path().to_path_buf()]
        );
        assert!(search_directories(None).is_empty());
    }

    #[test]
    fn executable_resolution_never_falls_back_to_unsafe_candidates() {
        let directory = tempfile::tempdir().expect("tempdir");
        let program = if cfg!(windows) {
            "fake-git.exe"
        } else {
            "fake-git"
        };
        let fake = directory.path().join(program);
        std::fs::write(&fake, b"not really git").expect("fake git");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        let search = directory.path().as_os_str();
        assert_eq!(
            resolve_executable(Path::new("fake-git"), Some(search)).expect("found"),
            fake
        );
        assert!(resolve_executable(Path::new("missing-git"), Some(search)).is_err());
        assert!(resolve_executable(Path::new("fake-git"), None).is_err());
        assert!(resolve_executable(Path::new("bin/fake-git"), Some(search)).is_err());
        assert_eq!(resolve_executable(&fake, None).expect("absolute"), fake);
        #[cfg(windows)]
        {
            std::fs::write(directory.path().join("fake-git.cmd"), b"@echo off").expect("cmd");
            assert!(resolve_executable(Path::new("fake-git.cmd"), Some(search)).is_err());
            assert!(resolve_executable(&directory.path().join("fake-git.cmd"), None).is_err());
        }
    }

    /// A runner that answers Git commands from a table instead of starting processes.
    fn scripted(
        responses: Vec<(&'static str, CommandOutput)>,
    ) -> impl Fn(Command, Duration) -> std::io::Result<CommandOutput> {
        let responses = std::sync::Mutex::new(responses);
        move |command: Command, _timeout: Duration| {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let mut responses = responses.lock().expect("responses");
            let position = responses
                .iter()
                .position(|(subcommand, _)| args.iter().any(|arg| arg == subcommand))
                .unwrap_or_else(|| panic!("unexpected Git command {args:?}"));
            Ok(responses.remove(position).1)
        }
    }

    fn finished(code: i32, stdout: &[u8]) -> CommandOutput {
        CommandOutput {
            code: Some(code),
            stdout: stdout.to_vec(),
            complete: true,
            ..CommandOutput::default()
        }
    }

    fn timed_out() -> CommandOutput {
        CommandOutput {
            timed_out: true,
            ..CommandOutput::default()
        }
    }

    #[test]
    fn timed_out_status_is_reported_as_unknown_never_clean() {
        let root = tempfile::tempdir().expect("root");
        let runner = scripted(vec![
            ("config", finished(1, b"")),
            ("status", timed_out()),
            ("symbolic-ref", finished(0, b"main\n")),
            ("log", finished(0, b"abc123\tLatest subject\n")),
        ]);
        let session = Session {
            executable: PathBuf::from("git"),
            timeout: Duration::from_millis(100),
            runner: &runner,
        };
        let snapshot = session.snapshot(root.path()).expect("snapshot");
        assert!(snapshot.timed_out);
        assert!(
            snapshot.dirty,
            "an unknown state must never be reported as clean"
        );
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.head_commit.as_deref(), Some("abc123"));
        assert_eq!(snapshot.last_commit.as_deref(), Some("Latest subject"));
        assert_eq!(snapshot.repository_root.as_deref(), Some(root.path()));

        let runner = scripted(vec![("config", finished(1, b"")), ("status", timed_out())]);
        let session = Session {
            runner: &runner,
            ..session
        };
        assert!(
            session
                .changed_files(root.path())
                .expect("files")
                .is_empty()
        );
    }

    #[test]
    fn unanswered_configuration_listing_is_unknown_and_failed_status_is_an_error() {
        let root = tempfile::tempdir().expect("root");
        let runner = scripted(vec![
            ("config", timed_out()),
            ("symbolic-ref", finished(1, b"")),
            ("log", timed_out()),
        ]);
        let session = Session {
            executable: PathBuf::from("git"),
            timeout: Duration::from_millis(100),
            runner: &runner,
        };
        let snapshot = session.snapshot(root.path()).expect("snapshot");
        assert!(snapshot.timed_out && snapshot.dirty && snapshot.detached_head);

        let mut failed = finished(128, b"");
        failed.stderr = "fatal: index file corrupt".into();
        let runner = scripted(vec![("config", finished(1, b"")), ("status", failed)]);
        let session = Session {
            runner: &runner,
            ..session
        };
        assert!(matches!(
            session.snapshot(root.path()),
            Err(ContextWakeError::Git(message)) if message.contains("index file corrupt")
        ));
    }

    #[test]
    fn discovery_requires_the_work_tree_to_contain_the_repository() {
        let root = tempfile::tempdir().expect("root");
        let repository = root.path().join("repository");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(repository.join(".git")).expect("git dir");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        let git_dir = conventional_path(&repository.join(".git"));
        assert!(work_tree_owns(&conventional_path(&repository), &git_dir));
        assert!(!work_tree_owns(&conventional_path(&elsewhere), &git_dir));

        let linked = root.path().join("linked");
        std::fs::create_dir_all(&linked).expect("linked");
        std::fs::write(linked.join(".git"), "gitdir: ../repository/.git\n").expect("gitfile");
        assert!(work_tree_owns(&conventional_path(&linked), &git_dir));
        std::fs::write(linked.join(".git"), "gitdir: ../elsewhere\n").expect("gitfile");
        assert!(!work_tree_owns(&conventional_path(&linked), &git_dir));
    }

    #[cfg(windows)]
    fn system32(program: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32")
            .join(program)
    }

    /// A command whose child inherits the output pipes and outlives it (`detach`) or keeps
    /// running while the command waits for it.
    #[cfg(windows)]
    fn descendant_holding_the_pipes(detach: bool, seconds: u32) -> Command {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(system32("cmd.exe"));
        command.raw_arg(format!(
            "/d /c {}{} -n {} 127.0.0.1",
            if detach { "start /b " } else { "" },
            system32("PING.EXE").display(),
            seconds + 1
        ));
        command
    }

    #[cfg(unix)]
    fn descendant_holding_the_pipes(detach: bool, seconds: u32) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(if detach {
            format!("sleep {seconds} &")
        } else {
            format!("sleep {seconds}; :")
        });
        command
    }

    #[test]
    fn timeout_is_bounded_when_a_descendant_holds_the_pipes() {
        let started = Instant::now();
        let output = run_bounded(
            descendant_holding_the_pipes(false, 30),
            Duration::from_millis(500),
        )
        .expect("run");
        assert!(output.timed_out && !output.complete);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited {:?} for a descendant",
            started.elapsed()
        );
    }

    /// Only stopping the whole tree lets the pipes close long before the 30-second
    /// descendant would exit, even with a generous pipe grace period.
    #[cfg(windows)]
    #[test]
    fn timeout_stops_the_whole_process_tree() {
        let started = Instant::now();
        let output = run_with_pipe_grace(
            descendant_holding_the_pipes(false, 30),
            Duration::from_millis(500),
            Duration::from_secs(25),
        )
        .expect("run");
        assert!(output.timed_out && !output.complete);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the descendant kept running for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn exited_command_whose_descendant_keeps_the_pipes_open_is_incomplete() {
        let started = Instant::now();
        let output = run_bounded(
            descendant_holding_the_pipes(true, 6),
            Duration::from_secs(5),
        )
        .expect("run");
        assert!(output.timed_out && !output.complete && !output.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    fn file_names(directory: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(directory)
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

    #[test]
    fn hardened_invocations_never_run_hooks_even_when_git_writes_the_index() {
        use std::io::Write as _;
        use std::time::SystemTime;

        let root = tempfile::tempdir().expect("root");
        let repository = root.path().join("repository");
        let markers = root.path().join("markers");
        std::fs::create_dir_all(&repository).expect("repository");
        std::fs::create_dir_all(&markers).expect("markers");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(&repository)
                .args(args)
                .stdin(Stdio::null())
                .status()
                .expect("Git must be installed for this test");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        let tracked = repository.join("tracked.txt");
        std::fs::write(&tracked, "tracked\n").expect("tracked file");
        git(&["add", "tracked.txt"]);
        git(&[
            "-c",
            "user.name=ContextWake Tests",
            "-c",
            "user.email=tests@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "initial",
        ]);

        // A hooks-directory hook and a configured hook for the event that index writes fire.
        let marker = |name: &str| markers.join(name).to_string_lossy().replace('\\', "/");
        let hooks = repository.join(".git").join("hooks");
        let hook = hooks.join("post-index-change");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\n", marker("hooks-directory")),
        )
        .expect("hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .expect("executable hook");
        }
        let mut config = std::fs::OpenOptions::new()
            .append(true)
            .open(repository.join(".git").join("config"))
            .expect("config");
        writeln!(
            config,
            "[hook \"audit\"]\n\tcommand = \"touch '{}' #\"\n\tevent = post-index-change",
            marker("configured")
        )
        .expect("configured hook");
        drop(config);

        // A stat-only change makes `update-index --refresh` rewrite the index.
        let backdate = |seconds: u64| {
            std::fs::File::options()
                .write(true)
                .open(&tracked)
                .expect("open tracked file")
                .set_modified(SystemTime::now() - Duration::from_secs(seconds))
                .expect("backdate tracked file");
        };
        backdate(3_600);
        let hooks_path = format!("core.hooksPath={}", hooks.display());
        git(&["-c", &hooks_path, "update-index", "-q", "--refresh"]);
        // Configured hooks only exist in recent Git releases; the hooks-directory hook
        // proves that the refresh rewrote the index.
        assert!(
            file_names(&markers).contains(&"hooks-directory".to_string()),
            "the fixture's hooks must fire without hardening"
        );
        for name in file_names(&markers) {
            std::fs::remove_file(markers.join(name)).expect("reset marker");
        }

        backdate(7_200);
        let session = Session {
            executable: resolve_executable(Path::new("git"), std::env::var_os("PATH").as_deref())
                .expect("Git on PATH"),
            timeout: Duration::from_secs(20),
            runner: &run_bounded,
        };
        let settings = session
            .neutralizing_settings(&repository)
            .expect("configuration listing")
            .expect("Git answered");
        let refresh = session
            .run(
                Some(&repository),
                &settings,
                &["update-index", "-q", "--refresh"],
            )
            .expect("refresh");
        assert!(refresh.success(), "{}", refresh.stderr);
        assert!(
            file_names(&markers).is_empty(),
            "hooks ran: {:?}",
            file_names(&markers)
        );
    }
}
