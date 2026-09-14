use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::error::{ContextWakeError, Result};
use crate::model::GitSnapshot;
use crate::security::sanitize_terminal;

#[derive(Clone, Debug)]
pub struct GitClient {
    executable: PathBuf,
    timeout: Duration,
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
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

    pub fn version(&self) -> Result<String> {
        let output = self.run(None, [OsStr::new("--version")])?;
        if !output.success {
            return Err(ContextWakeError::Git(actionable_git_error(&output.stderr)));
        }
        Ok(sanitize_terminal(output.stdout.trim()))
    }

    pub fn repository_root(&self, path: &Path) -> Result<Option<PathBuf>> {
        let output = self.run(
            Some(path),
            [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        )?;
        if !output.success {
            return Ok(None);
        }
        let root = PathBuf::from(output.stdout.trim());
        Ok(Some(root.canonicalize().unwrap_or(root)))
    }

    pub fn snapshot(&self, path: &Path) -> Result<Option<GitSnapshot>> {
        let Some(root) = self.repository_root(path)? else {
            return Ok(None);
        };
        let status = self.run(
            Some(&root),
            [
                OsStr::new("status"),
                OsStr::new("--porcelain=v2"),
                OsStr::new("--branch"),
                OsStr::new("--untracked-files=all"),
            ],
        )?;
        if status.timed_out {
            return Ok(Some(GitSnapshot {
                repository_root: Some(root),
                timed_out: true,
                ..GitSnapshot::default()
            }));
        }
        if !status.success {
            return Err(ContextWakeError::Git(actionable_git_error(&status.stderr)));
        }

        let mut snapshot = parse_porcelain_v2(&status.stdout);
        snapshot.repository_root = Some(root.clone());

        let diff = self.run(
            Some(&root),
            [
                OsStr::new("diff"),
                OsStr::new("--numstat"),
                OsStr::new("HEAD"),
            ],
        )?;
        if diff.success {
            (snapshot.additions, snapshot.deletions) = parse_numstat(&diff.stdout);
        }

        let commit = self.run(
            Some(&root),
            [
                OsStr::new("log"),
                OsStr::new("-1"),
                OsStr::new("--format=%H%x09%s"),
            ],
        )?;
        if commit.success {
            let line = sanitize_terminal(commit.stdout.trim());
            let mut parts = line.splitn(2, '\t');
            snapshot.head_commit = parts.next().map(ToOwned::to_owned);
            snapshot.last_commit = parts.next().map(ToOwned::to_owned);
        }

        snapshot.dirty = snapshot.staged_count > 0
            || snapshot.unstaged_count > 0
            || snapshot.untracked_count > 0;
        Ok(Some(snapshot))
    }

    pub fn changed_files(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let Some(root) = self.repository_root(path)? else {
            return Ok(Vec::new());
        };
        let commands: [&[&str]; 3] = [
            &["diff", "--name-only", "-z"],
            &["diff", "--cached", "--name-only", "-z"],
            &["ls-files", "--others", "--exclude-standard", "-z"],
        ];
        let mut files = BTreeSet::new();
        for args in commands {
            let output = self.run(Some(&root), args.iter().map(OsStr::new))?;
            if output.timed_out {
                break;
            }
            if !output.success {
                continue;
            }
            for raw in output.stdout.split('\0').filter(|value| !value.is_empty()) {
                let path = PathBuf::from(sanitize_terminal(raw));
                if !path.is_absolute()
                    && !path.components().any(|part| {
                        matches!(
                            part,
                            std::path::Component::ParentDir | std::path::Component::Prefix(_)
                        )
                    })
                {
                    files.insert(path);
                }
            }
        }
        Ok(files.into_iter().collect())
    }

    fn run<I, S>(&self, cwd: Option<&Path>, args: I) -> Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| {
            ContextWakeError::Git(format!(
                "could not start {}: {error}. Install Git or configure its executable path.",
                self.executable.display()
            ))
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ContextWakeError::Git("could not capture Git standard output".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ContextWakeError::Git("could not capture Git diagnostic output".into())
        })?;
        let stdout_reader = thread::spawn(move || read_stream(stdout));
        let stderr_reader = thread::spawn(move || read_stream(stderr));

        let status = child
            .wait_timeout(self.timeout)
            .map_err(|error| ContextWakeError::Git(format!("could not wait for Git: {error}")))?;
        let (success, timed_out) = if let Some(status) = status {
            (status.success(), false)
        } else {
            let _ = child.kill();
            let _ = child.wait();
            (false, true)
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| ContextWakeError::Git("Git output reader failed".into()))?
            .map_err(|error| {
                ContextWakeError::Git(format!("could not read Git output: {error}"))
            })?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| ContextWakeError::Git("Git diagnostic reader failed".into()))?
            .map_err(|error| {
                ContextWakeError::Git(format!("could not read Git diagnostics: {error}"))
            })?;
        Ok(CommandOutput {
            success,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: sanitize_terminal(&String::from_utf8_lossy(&stderr)),
            timed_out,
        })
    }
}

fn read_stream(mut stream: impl Read) -> std::io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn actionable_git_error(stderr: &str) -> String {
    let message = sanitize_terminal(stderr.trim());
    if message.is_empty() {
        "Git returned a non-zero status. Run `ctx doctor` for details.".into()
    } else {
        message
    }
}

fn parse_porcelain_v2(output: &str) -> GitSnapshot {
    let mut snapshot = GitSnapshot::default();
    for raw_line in output.lines() {
        let line = sanitize_terminal(raw_line);
        if let Some(value) = line.strip_prefix("# branch.head ") {
            if value == "(detached)" {
                snapshot.detached_head = true;
            } else {
                snapshot.branch = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("# branch.oid ") {
            if value != "(initial)" {
                snapshot.head_commit = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("# branch.ab ") {
            for part in value.split_whitespace() {
                if let Some(ahead) = part.strip_prefix('+') {
                    snapshot.ahead = ahead.parse().ok();
                } else if let Some(behind) = part.strip_prefix('-') {
                    snapshot.behind = behind.parse().ok();
                }
            }
        } else if line.starts_with("? ") {
            snapshot.untracked_count += 1;
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            if let Some(xy) = line.split_whitespace().nth(1) {
                let mut chars = xy.chars();
                let index = chars.next().unwrap_or('.');
                let worktree = chars.next().unwrap_or('.');
                if index != '.' {
                    snapshot.staged_count += 1;
                }
                if worktree != '.' {
                    snapshot.unstaged_count += 1;
                }
            }
        } else if line.starts_with("u ") {
            snapshot.staged_count += 1;
            snapshot.unstaged_count += 1;
        }
    }
    snapshot
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
        let snapshot = parse_porcelain_v2(
            "# branch.oid abc123\n# branch.head feature/test\n# branch.ab +2 -1\n1 M. N... file\n1 .M N... other\n? untracked\n",
        );
        assert_eq!(snapshot.branch.as_deref(), Some("feature/test"));
        assert_eq!(snapshot.ahead, Some(2));
        assert_eq!(snapshot.behind, Some(1));
        assert_eq!(snapshot.staged_count, 1);
        assert_eq!(snapshot.unstaged_count, 1);
        assert_eq!(snapshot.untracked_count, 1);
    }

    #[test]
    fn parses_text_and_binary_numstat() {
        assert_eq!(parse_numstat("12\t4\tsrc/a.rs\n-\t-\timage.png\n"), (12, 4));
    }
}
