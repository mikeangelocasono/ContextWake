use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::security::{SecretScanner, sanitize_terminal};

pub(crate) const MAX_PROBE_OUTPUT: usize = 65_536;

#[derive(Debug)]
pub(crate) struct ProbeOutput {
    pub(crate) success: bool,
    pub(crate) timed_out: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn safe_agent_text(value: &str) -> String {
    let redacted = SecretScanner::new().redact(&sanitize_terminal(value)).text;
    compact_agent_text(&redacted, 512)
        .unwrap_or_else(|| "Provider returned no diagnostic output".into())
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

pub(crate) fn run_probe(mut command: Command, timeout: Duration) -> std::io::Result<ProbeOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("provider probe standard output was not captured"))?;
    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::other("provider probe diagnostic output was not captured")
    })?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let status = child.wait_timeout(timeout)?;
    let (success, timed_out) = if let Some(status) = status {
        (status.success(), false)
    } else {
        let _ = child.kill();
        let _ = child.wait();
        (false, true)
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("provider output reader failed"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("provider diagnostic reader failed"))??;
    Ok(ProbeOutput {
        success,
        timed_out,
        stdout,
        stderr,
    })
}

fn read_bounded(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8_192];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_PROBE_OUTPUT.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..count.min(remaining)]);
    }
    Ok(output)
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

pub(crate) fn contained_existing_path(root: &Path, candidate: &Path) -> Option<std::path::PathBuf> {
    let root = root.canonicalize().ok()?;
    let candidate = candidate.canonicalize().ok()?;
    candidate.starts_with(&root).then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let output = run_probe(command, Duration::from_secs(5)).expect("probe");
        assert!(!output.success);
        assert!(!output.timed_out);
        assert!(output.stdout.len() <= MAX_PROBE_OUTPUT);
        assert!(output.stderr.len() <= MAX_PROBE_OUTPUT);
    }
}
