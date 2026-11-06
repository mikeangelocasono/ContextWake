use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::model::AuthState;
use crate::paths::{canonicalize_simplified, path_is_within, paths_equivalent};
use crate::security::{SecretScanner, sanitize_terminal};

/// Standard-output bound for version, help and authentication probes.
pub(crate) const MAX_PROBE_OUTPUT: usize = 64 * 1024;
/// Standard-output bound for machine-readable session and model listings.
pub(crate) const MAX_LISTING_OUTPUT: usize = 16 * 1024 * 1024;
/// Standard-error bound; it only feeds bounded diagnostics.
const MAX_DIAGNOSTIC_OUTPUT: usize = 64 * 1024;
/// How long output pipes may stay open after the probe exited or was killed.
/// A descendant that inherited them (an npm launcher's native child, a
/// background helper) is not allowed to extend a probe indefinitely.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Bound for reaping a killed probe and for the process-tree kill helper.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// Whether the captured standard output is everything the probe wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputCompleteness {
    #[default]
    Complete,
    /// Standard output exceeded the caller's limit; the rest was discarded.
    Truncated,
    /// A descendant still held the output pipes after the probe ended, so the
    /// captured output may be incomplete.
    PipesHeldOpen,
}

#[derive(Debug, Default)]
pub(crate) struct ProbeOutput {
    pub(crate) success: bool,
    pub(crate) timed_out: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) completeness: OutputCompleteness,
}

impl ProbeOutput {
    /// Machine-readable listings are parsed only from complete output; a cut
    /// prefix would turn into a parse error or, worse, a bogus last record.
    pub(crate) fn require_complete_stdout(
        &self,
        operation: &str,
        limit: usize,
        hint: Option<&str>,
    ) -> Result<()> {
        let hint = hint.map(|hint| format!("; {hint}")).unwrap_or_default();
        match self.completeness {
            OutputCompleteness::Complete => Ok(()),
            OutputCompleteness::Truncated => Err(ContextWakeError::Provider(format!(
                "{operation} produced more than {} of output, ContextWake's safety limit, so nothing was parsed{hint}",
                describe_bytes(limit)
            ))),
            OutputCompleteness::PipesHeldOpen => Err(ContextWakeError::Provider(format!(
                "{operation} exited while another process still held its output open, so the output may be incomplete and nothing was parsed{hint}"
            ))),
        }
    }
}

fn describe_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes % MIB == 0 {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes % KIB == 0 {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Terminal-safe, redacted, single-line provider text capped at 512
/// characters, or `None` when nothing visible remains.
pub(crate) fn safe_agent_label(value: &str) -> Option<String> {
    let redacted = SecretScanner::new().redact(&sanitize_terminal(value)).text;
    compact_agent_text(&redacted, 512)
}

pub(crate) fn safe_agent_text(value: &str) -> String {
    safe_agent_label(value).unwrap_or_else(|| "Provider returned no diagnostic output".into())
}

pub(crate) fn safe_probe_diagnostic(output: &ProbeOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let source = if stderr.trim().is_empty() {
        stdout.as_ref()
    } else {
        stderr.as_ref()
    };
    let redacted = SecretScanner::new().redact(&sanitize_terminal(source)).text;
    let selected = redacted
        .lines()
        .find(|line| line.trim_start().to_ascii_lowercase().starts_with("error:"))
        .or_else(|| redacted.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default();
    compact_agent_text(selected, 512)
        .unwrap_or_else(|| "Provider probe failed without diagnostic output".into())
}

fn compact_agent_text(value: &str, max_characters: usize) -> Option<String> {
    let compact = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_characters)
        .collect::<String>();
    (!compact.is_empty()).then_some(compact)
}

/// Authentication is advisory while detecting an agent: a failing auth probe
/// makes the state `Unknown` and is mentioned in `message` instead of failing
/// detection (and with it `ctx status`, `ctx agent detect` and `ctx profile
/// use`).
pub(crate) fn advisory_auth_state(
    result: Option<Result<AuthState>>,
    message: &mut String,
) -> AuthState {
    match result {
        None => AuthState::Unknown,
        Some(Ok(state)) => state,
        Some(Err(error)) => {
            message.push_str("; authentication status is unknown: ");
            message.push_str(&safe_agent_text(&error.to_string()));
            AuthState::Unknown
        }
    }
}

/// Runs a short, non-interactive provider probe with hard bounds:
///
/// * standard input is the null device, so a probe never competes with the
///   TUI for keystrokes or waits on a prompt;
/// * at most `max_stdout` bytes of standard output are kept, and truncation is
///   reported in [`ProbeOutput::completeness`];
/// * on timeout the whole process tree is killed (`taskkill /T /F` on
///   Windows, the probe's own process group elsewhere), not only the direct
///   child;
/// * output pipes that a descendant keeps open are abandoned after a short
///   grace period, so no wait is unbounded.
pub(crate) fn run_probe(
    mut command: Command,
    timeout: Duration,
    max_stdout: usize,
) -> std::io::Result<ProbeOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group lets a timeout signal every descendant that
        // shares the pipes.
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let (finished_sender, finished) = mpsc::channel();
    let captures = child
        .stdout
        .take()
        .zip(child.stderr.take())
        .ok_or_else(|| std::io::Error::other("provider probe output was not captured"))
        .and_then(|(stdout, stderr)| {
            let stdout = Capture::start(stdout, max_stdout, finished_sender.clone())?;
            let stderr = Capture::start(stderr, MAX_DIAGNOSTIC_OUTPUT, finished_sender)?;
            Ok((stdout, stderr))
        });
    let (stdout, stderr) = match captures {
        Ok(captures) => captures,
        Err(error) => {
            terminate_process_tree(&mut child);
            return Err(error);
        }
    };
    let (success, timed_out) = match child.wait_timeout(timeout) {
        Ok(Some(status)) => (status.success(), false),
        Ok(None) => {
            terminate_process_tree(&mut child);
            (false, true)
        }
        Err(error) => {
            terminate_process_tree(&mut child);
            return Err(error);
        }
    };
    // On Windows a descendant that outlives a successful probe and keeps the
    // pipes is not killed: its parent has exited, so `taskkill /T` can no
    // longer find the tree (that needs a Job Object). Its reader threads are
    // abandoned instead. Probes never run npm `.cmd` shims there.
    let drained = wait_for_pipes(&finished, 2, PIPE_DRAIN_GRACE);
    let pipes_held_open = drained < 2;
    #[cfg(unix)]
    if pipes_held_open {
        // The probe is gone but a descendant in its process group still holds
        // the pipes; stop it so the reader threads can finish. Accepted
        // residual risk: the leader is already reaped, so if every holder
        // left the group (setsid) the group is empty and its ID could in
        // principle be reused within the grace period. Closing that window
        // needs `waitid(WNOWAIT)` from libc, which this crate does not use.
        signal_process_group(child.id());
        let _ = wait_for_pipes(&finished, 2 - drained, PIPE_DRAIN_GRACE);
    }
    let (stdout, stdout_truncated, stdout_error) = stdout.finish();
    let (stderr, _, stderr_error) = stderr.finish();
    if let Some(error) = stdout_error.or(stderr_error) {
        return Err(error);
    }
    let completeness = if stdout_truncated {
        OutputCompleteness::Truncated
    } else if pipes_held_open {
        OutputCompleteness::PipesHeldOpen
    } else {
        OutputCompleteness::Complete
    };
    Ok(ProbeOutput {
        success,
        timed_out,
        stdout,
        stderr,
        completeness,
    })
}

/// Runs a machine-readable listing probe and returns its complete stdout.
pub(crate) fn run_listing(
    command: Command,
    timeout: Duration,
    limit: usize,
    operation: &str,
    hint: Option<&str>,
) -> Result<Vec<u8>> {
    let output = run_probe(command, timeout, limit).map_err(|error| {
        ContextWakeError::Provider(format!("could not run {operation}: {error}"))
    })?;
    if output.timed_out {
        return Err(ContextWakeError::Provider(format!(
            "{operation} timed out after {} seconds",
            timeout.as_secs()
        )));
    }
    if !output.success {
        return Err(ContextWakeError::Provider(format!(
            "{operation} failed: {}",
            safe_probe_diagnostic(&output)
        )));
    }
    output.require_complete_stdout(operation, limit, hint)?;
    Ok(output.stdout)
}

fn wait_for_pipes(finished: &mpsc::Receiver<()>, expected: usize, grace: Duration) -> usize {
    let deadline = Instant::now() + grace;
    let mut count = 0;
    while count < expected {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if finished.recv_timeout(remaining).is_err() {
            break;
        }
        count += 1;
    }
    count
}

#[derive(Default)]
struct CaptureState {
    bytes: Vec<u8>,
    truncated: bool,
    error: Option<std::io::Error>,
    abandoned: bool,
}

/// Reads one output pipe on its own thread into a bounded buffer that the
/// probe can collect at any time, even while the pipe is still open.
struct Capture {
    state: Arc<Mutex<CaptureState>>,
}

/// Signals that a reader thread ended, including by panic.
struct Finished(mpsc::Sender<()>);

impl Drop for Finished {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn lock(state: &Mutex<CaptureState>) -> MutexGuard<'_, CaptureState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Capture {
    fn start(
        mut reader: impl Read + Send + 'static,
        limit: usize,
        finished: mpsc::Sender<()>,
    ) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(CaptureState::default()));
        let shared = Arc::clone(&state);
        thread::Builder::new()
            .name("contextwake-probe-output".into())
            .spawn(move || {
                let _finished = Finished(finished);
                let mut chunk = [0_u8; 8_192];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => {
                            let mut state = lock(&shared);
                            if state.abandoned {
                                break;
                            }
                            let kept = count.min(limit.saturating_sub(state.bytes.len()));
                            state.bytes.extend_from_slice(&chunk[..kept]);
                            state.truncated |= kept < count;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            lock(&shared).error = Some(error);
                            break;
                        }
                    }
                }
            })?;
        Ok(Self { state })
    }

    /// Takes what was captured. A reader that is still blocked on a pipe held
    /// by an escaped descendant stops at its next read and drops the pipe.
    fn finish(self) -> (Vec<u8>, bool, Option<std::io::Error>) {
        let mut state = lock(&self.state);
        state.abandoned = true;
        (
            std::mem::take(&mut state.bytes),
            state.truncated,
            state.error.take(),
        )
    }
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    kill_windows_process_tree(child.id());
    #[cfg(unix)]
    signal_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait_timeout(KILL_WAIT);
}

/// `taskkill /T /F` ends the probe and every process it started. The probe's
/// process handle is still open here, so its PID cannot have been reused.
#[cfg(windows)]
fn kill_windows_process_tree(pid: u32) {
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut taskkill = Command::new(system_root.join("System32").join("taskkill.exe"));
    taskkill.args(["/PID", &pid.to_string(), "/T", "/F"]);
    run_bounded_helper(&mut taskkill);
}

/// Sends SIGKILL to the probe's process group. The group exists while any
/// member is alive, and its ID is not reused meanwhile.
#[cfg(unix)]
fn signal_process_group(pid: u32) {
    let Some(kill) = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
    else {
        return;
    };
    let mut command = Command::new(kill);
    command.args(["-9", &format!("-{pid}")]);
    run_bounded_helper(&mut command);
}

fn run_bounded_helper(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut helper) = command.spawn()
        && !matches!(helper.wait_timeout(KILL_WAIT), Ok(Some(_)))
    {
        let _ = helper.kill();
        let _ = helper.wait_timeout(KILL_WAIT);
    }
}

/// Matches provider-recorded session directories against one workspace the
/// same way `ctx session sync` does (resolved paths, compared with the
/// platform's rules), caching per distinct directory.
pub(crate) struct WorkspaceMatcher {
    literal: PathBuf,
    resolved: Option<PathBuf>,
    cache: HashMap<PathBuf, bool>,
}

impl WorkspaceMatcher {
    pub(crate) fn new(workspace: &Path) -> Self {
        Self {
            literal: workspace.to_path_buf(),
            resolved: canonicalize_simplified(workspace).ok(),
            cache: HashMap::new(),
        }
    }

    pub(crate) fn matches(&mut self, recorded: &Path) -> bool {
        if let Some(known) = self.cache.get(recorded) {
            return *known;
        }
        let matches = recorded.is_absolute()
            && match (&self.resolved, canonicalize_simplified(recorded)) {
                (Some(workspace), Ok(candidate)) => paths_equivalent(&candidate, workspace),
                (None, _) => paths_equivalent(recorded, &self.literal),
                (Some(_), Err(_)) => false,
            };
        self.cache.insert(recorded.to_path_buf(), matches);
        matches
    }
}

pub(crate) fn read_file_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file = File::open(path).at(path)?;
    let limit = u64::try_from(max_bytes)
        .map_err(|_| ContextWakeError::InvalidData("provider file limit is invalid".into()))?;
    let mut reader = file.take(limit.saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).at(path)?;
    if bytes.len() > max_bytes {
        return Err(ContextWakeError::InvalidData(format!(
            "provider metadata exceeds the {max_bytes}-byte safety limit: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(crate) fn read_file_prefix(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file = File::open(path).at(path)?;
    let limit = u64::try_from(max_bytes)
        .map_err(|_| ContextWakeError::InvalidData("provider file limit is invalid".into()))?;
    let mut reader = file.take(limit);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).at(path)?;
    Ok(bytes)
}

pub(crate) fn ensure_profile_directory(path: &Path, label: &str) -> Result<()> {
    std::fs::create_dir_all(path).at(path)?;
    let metadata = std::fs::symlink_metadata(path).at(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ContextWakeError::UnsafePath(format!(
            "{label} profile home must be a real directory: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).at(path)?;
    }
    Ok(())
}

pub(crate) fn contained_existing_path(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let root = canonicalize_simplified(root).ok()?;
    let candidate = canonicalize_simplified(candidate).ok()?;
    path_is_within(&candidate, &root).then_some(candidate)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fake agent that is a script interpreted by the platform's command
    /// processor, so tests need no compiled helper binaries. On Windows the
    /// script is a batch file run by `cmd.exe /d /c` (tests only; production
    /// code never runs batch files); elsewhere it is a `sh` script.
    pub(crate) fn fake_agent(
        directory: &Path,
        windows_batch: &str,
        unix_sh: &str,
    ) -> crate::provider::discovery::Launcher {
        #[cfg(windows)]
        {
            let _ = unix_sh;
            let script = directory.join("fake-agent.cmd");
            std::fs::write(
                &script,
                format!("@echo off\r\n{}\r\n", windows_batch.replace('\n', "\r\n")),
            )
            .expect("fake agent script");
            let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
            crate::provider::discovery::Launcher::with_arguments(
                PathBuf::from(system_root).join(r"System32\cmd.exe"),
                [
                    std::ffi::OsString::from("/d"),
                    "/c".into(),
                    script.into_os_string(),
                ],
            )
        }
        #[cfg(not(windows))]
        {
            let _ = windows_batch;
            let script = directory.join("fake-agent.sh");
            std::fs::write(&script, format!("set -eu\n{unix_sh}\n")).expect("fake agent script");
            crate::provider::discovery::Launcher::with_arguments(
                "/bin/sh",
                [script.into_os_string()],
            )
        }
    }

    fn system_command(windows: &str, unix: &str) -> Command {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = unix;
            let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
            let mut command = Command::new(PathBuf::from(system_root).join(r"System32\cmd.exe"));
            command.raw_arg(format!("/d /c \"{windows}\""));
            command
        }
        #[cfg(not(windows))]
        {
            let _ = windows;
            let mut command = Command::new("/bin/sh");
            command.args(["-c", unix]);
            command
        }
    }

    #[test]
    fn terminal_text_is_unicode_safe_bounded_and_redacted() {
        let safe = safe_agent_text(
            "\u{1b}[31m Unicode café 🔧 sk-abcdefghijklmnopqrstuvwxyz\nsecond line",
        );
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains("sk-"));
        assert!(safe.contains("Unicode café 🔧"));
        assert!(!safe.contains('\n'));
        assert!(safe.chars().count() <= 512);
    }

    #[test]
    fn empty_provider_labels_stay_empty() {
        assert_eq!(safe_agent_label(""), None);
        assert_eq!(safe_agent_label(" \t\r\n "), None);
        assert_eq!(safe_agent_label("\u{1b}[31m\u{1b}[0m"), None);
        assert_eq!(safe_agent_label(" CSV\nexport "), Some("CSV export".into()));
    }

    #[test]
    fn bounded_files_reject_oversized_provider_output() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("metadata.json");
        std::fs::write(&path, vec![b'x'; 33]).expect("fixture");
        assert!(read_file_bounded(&path, 32).is_err());
    }

    #[test]
    fn provider_process_failure_is_bounded_and_reported() {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command.arg("--definitely-not-a-rust-test-harness-option");
        let output = run_probe(command, Duration::from_secs(5), MAX_PROBE_OUTPUT).expect("probe");
        assert!(!output.success);
        assert!(!output.timed_out);
        assert!(output.stdout.len() <= MAX_PROBE_OUTPUT);
        assert!(output.stderr.len() <= MAX_PROBE_OUTPUT);
    }

    #[test]
    fn output_beyond_the_callers_limit_is_reported_as_truncated() {
        let command = || {
            system_command(
                "for /L %i in (1,1,2000) do @echo 0123456789012345678901234567890123456789",
                "i=0; while [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done",
            )
        };
        let small = run_probe(command(), Duration::from_secs(30), 4_096).expect("probe");
        assert!(small.success);
        assert_eq!(small.completeness, OutputCompleteness::Truncated);
        assert_eq!(small.stdout.len(), 4_096);
        let error = small
            .require_complete_stdout("fixture listing", 4_096, Some("retry with fewer records"))
            .expect_err("truncated output is never parsed");
        assert!(error.to_string().contains("4 KiB"), "{error}");
        assert!(
            error.to_string().contains("retry with fewer records"),
            "{error}"
        );

        let large =
            run_probe(command(), Duration::from_secs(30), MAX_LISTING_OUTPUT).expect("probe");
        assert!(large.success);
        assert_eq!(large.completeness, OutputCompleteness::Complete);
        assert!(
            large.stdout.len() > MAX_PROBE_OUTPUT,
            "{}",
            large.stdout.len()
        );
        assert!(
            large
                .require_complete_stdout("fixture listing", MAX_LISTING_OUTPUT, None)
                .is_ok()
        );
    }

    #[test]
    fn timeout_kills_descendants_that_hold_the_output_pipes() {
        // The direct child waits on a grandchild that inherited stdout and
        // stderr. Killing only the child left the readers blocked until the
        // grandchild exited on its own (about 30 seconds here).
        let command = system_command("ping -n 30 127.0.0.1", "sleep 30 & wait");
        let started = Instant::now();
        let output =
            run_probe(command, Duration::from_millis(500), MAX_PROBE_OUTPUT).expect("probe");
        let elapsed = started.elapsed();
        assert!(output.timed_out);
        assert!(!output.success);
        assert!(elapsed < Duration::from_secs(15), "probe took {elapsed:?}");
        // The pipes closed, so the grandchild was killed, not just abandoned.
        assert_eq!(output.completeness, OutputCompleteness::Complete);
    }

    #[test]
    fn descendants_cannot_hold_a_finished_probe_open() {
        // The probe exits at once, but a background descendant keeps the
        // pipes open for about 12 seconds.
        let command = system_command(
            "start /b ping -n 12 127.0.0.1 & echo done",
            "sleep 12 & echo done",
        );
        let started = Instant::now();
        let output = run_probe(command, Duration::from_secs(10), MAX_PROBE_OUTPUT).expect("probe");
        let elapsed = started.elapsed();
        assert!(output.success);
        assert!(!output.timed_out);
        assert_eq!(output.completeness, OutputCompleteness::PipesHeldOpen);
        assert!(String::from_utf8_lossy(&output.stdout).contains("done"));
        assert!(elapsed < Duration::from_secs(9), "probe took {elapsed:?}");
        assert!(
            output
                .require_complete_stdout("fixture listing", MAX_PROBE_OUTPUT, None)
                .is_err()
        );
    }

    const STDIN_HELPER: &str = "CONTEXTWAKE_TEST_STDIN_PROBE_HELPER";

    /// Runs in a child test process whose standard input is a pipe that the
    /// parent keeps open. A probe that inherited it would block until timeout.
    #[test]
    #[ignore = "helper process for probes_never_inherit_standard_input"]
    fn stdin_probe_helper() {
        if std::env::var_os(STDIN_HELPER).is_none() {
            return;
        }
        #[cfg(windows)]
        let command = {
            let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
            Command::new(PathBuf::from(system_root).join(r"System32\sort.exe"))
        };
        #[cfg(not(windows))]
        let command = Command::new("cat");
        let output = run_probe(command, Duration::from_secs(5), MAX_PROBE_OUTPUT).expect("probe");
        println!(
            "STDIN-PROBE-RESULT: {}",
            if output.timed_out {
                "inherited"
            } else {
                "null"
            }
        );
    }

    #[test]
    fn probes_never_inherit_standard_input() {
        let mut helper = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "provider::common::tests::stdin_probe_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STDIN_HELPER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper");
        // Keep the helper's standard input open (and silent) until it exits.
        let open_stdin = helper.stdin.take();
        let status = helper
            .wait_timeout(Duration::from_secs(60))
            .expect("wait for helper");
        if status.is_none() {
            let _ = helper.kill();
            let _ = helper.wait();
        }
        drop(open_stdin);
        let mut stdout = String::new();
        helper
            .stdout
            .take()
            .expect("helper stdout")
            .read_to_string(&mut stdout)
            .expect("helper output");
        assert!(status.is_some_and(|status| status.success()), "{stdout}");
        assert!(stdout.contains("STDIN-PROBE-RESULT: null"), "{stdout}");
    }

    #[test]
    fn listing_failures_and_timeouts_are_explicit_errors() {
        let directory = tempfile::tempdir().expect("tempdir");
        let failing = fake_agent(
            directory.path(),
            "echo error: no such listing 1>&2\nexit /b 3",
            "echo 'error: no such listing' >&2; exit 3",
        );
        let error = run_listing(
            failing.command(),
            Duration::from_secs(10),
            MAX_LISTING_OUTPUT,
            "fixture listing",
            None,
        )
        .expect_err("failing listing");
        assert!(
            error.to_string().contains("fixture listing failed"),
            "{error}"
        );
        assert!(error.to_string().contains("no such listing"), "{error}");

        // A listing that hangs is reported as a timeout (Kiro's used to be
        // reported as a generic failure with whatever partial stderr existed).
        let slow_directory = directory.path().join("slow");
        std::fs::create_dir_all(&slow_directory).expect("slow fixture directory");
        let slow = fake_agent(&slow_directory, "ping -n 6 127.0.0.1 >nul", "sleep 5");
        let started = Instant::now();
        let error = run_listing(
            slow.command(),
            Duration::from_secs(1),
            MAX_LISTING_OUTPUT,
            "fixture listing",
            None,
        )
        .expect_err("hanging listing");
        assert!(
            error
                .to_string()
                .contains("fixture listing timed out after 1 seconds"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn workspace_matcher_compares_resolved_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = directory.path().join("Repo");
        let other = directory.path().join("Other");
        std::fs::create_dir_all(workspace.join("sub")).expect("workspace");
        std::fs::create_dir_all(&other).expect("other");
        let mut matcher = WorkspaceMatcher::new(&workspace);
        assert!(matcher.matches(&workspace));
        assert!(matcher.matches(&workspace.join("sub").join("..")));
        assert!(!matcher.matches(&other));
        assert!(!matcher.matches(&workspace.join("sub")));
        assert!(!matcher.matches(&directory.path().join("missing")));
        assert!(!matcher.matches(Path::new("relative")));
        #[cfg(windows)]
        assert!(matcher.matches(Path::new(&workspace.to_string_lossy().to_uppercase())));
    }

    #[test]
    fn advisory_authentication_never_fails_detection() {
        let mut message = String::from("Agent detected");
        assert_eq!(advisory_auth_state(None, &mut message), AuthState::Unknown);
        assert_eq!(
            advisory_auth_state(Some(Ok(AuthState::SignedIn)), &mut message),
            AuthState::SignedIn
        );
        assert_eq!(message, "Agent detected");
        let state = advisory_auth_state(
            Some(Err(ContextWakeError::Provider(
                "auth probe could not start".into(),
            ))),
            &mut message,
        );
        assert_eq!(state, AuthState::Unknown);
        assert!(
            message.contains("authentication status is unknown"),
            "{message}"
        );
    }
}
