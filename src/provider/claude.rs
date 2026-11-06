use std::cmp::Reverse;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::error::{ContextWakeError, IoContext, Result};
use crate::model::{
    AgentCapabilities, AgentHealth, AuthState, Capability, CapabilityMaturity, CapabilitySupport,
    ConversationExcerpt, DiscoveredAgentSession, ModelProvider,
};
use crate::paths::{canonicalize_simplified, paths_equivalent, simplify_path};
use crate::provider::common::{
    MAX_PROBE_OUTPUT, ProbeOutput, TranscriptMessages, WorkspaceMatcher, advisory_auth_state,
    conversation_excerpt_from, read_file_prefix, read_file_tail, run_probe, safe_agent_label,
    safe_agent_text, safe_probe_diagnostic,
};
use crate::provider::discovery::{AgentBinary, AgentExecutable, DiscoveryContext, Launcher};
use crate::provider::{AgentAdapter, HandoffLaunch, LaunchRecord, LaunchedSession};
use crate::security::{validate_external_reference, validate_session_reference};

/// Generous enough for Windows' first-run scan of the ~340 MB native binary.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

const BINARY: AgentBinary = AgentBinary {
    name: "Claude Code",
    install_hint: "Install Claude Code",
    variables: &["CONTEXTWAKE_CLAUDE_BIN", "AGENTDECK_CLAUDE_BIN"],
};

/// The variable that moves Claude Code's configuration home.
const CONFIG_DIR_VARIABLE: &str = "CLAUDE_CONFIG_DIR";
/// The Claude Code release whose launch and transcript contracts were
/// verified on Windows; the argv contract tests below pin that behavior.
const VERIFIED_VERSION: &str = "Claude Code 2.1.319";

/// Transcripts live in `<config home>\projects\<encoded cwd>\<session>.jsonl`.
const PROJECTS_DIRECTORY: &str = "projects";
/// The first line of every prompt `ContextWake` delivers starts with this.
const HANDOFF_PROMPT_PREFIX: &str = "ContextWake handoff ";
/// How a delivered `ContextWake` handoff prompt appears among the user
/// requests of a conversation excerpt (the prompt itself is long and is
/// already part of the handoff the next agent receives). Session titles skip
/// it and use the next typed request instead.
const DELIVERED_HANDOFF_REQUEST: &str = "[ContextWake handoff delivered]";
/// Claude Code text that is recorded as a user message but was not typed as
/// a request: slash-command records, local command and shell output, and
/// injected reminders.
const INJECTED_USER_TEXT: [&str; 6] = [
    "<command-",
    "<local-command-",
    "<bash-",
    "<system-reminder>",
    "<user-memory-input>",
    "[Request interrupted by user",
];

/// Encoded project directory names longer than this may be shortened by
/// Claude Code; such workspaces also match directories with this prefix
/// (confirmed by the transcripts' recorded `cwd`, like every match).
const MAX_ENCODED_NAME: usize = 200;
/// Directory entries read from one project (or the projects) directory.
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
/// Transcripts considered for one workspace, newest first by modification.
const MAX_TRANSCRIPT_FILES: usize = 256;
/// Head read from each transcript for its metadata (cwd, start, title).
const LISTING_HEAD_BYTES: usize = 256 * 1024;
/// Tail read from each transcript for its recency and latest title.
const LISTING_TAIL_BYTES: usize = 256 * 1024;
/// Tail read for a conversation excerpt.
const EXCERPT_TAIL_BYTES: usize = 2 * 1024 * 1024;
/// How much of the end of a transcript is searched for a delivered prompt.
const DELIVERY_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// Longer transcript lines are skipped while searching for a prompt.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Recently modified transcripts searched for a launch's prompt.
const MAX_LAUNCH_CANDIDATES: usize = 32;
/// Tolerated difference between `ContextWake`'s and Claude Code's clocks.
const CLOCK_SKEW_SECONDS: i64 = 2;
/// Where Claude Code keeps its configuration when `ContextWake` sets no
/// override of its own.
#[derive(Clone, Debug, Default)]
struct ConfigHomes {
    /// The absolute `CLAUDE_CONFIG_DIR` of the environment `ContextWake`
    /// runs in (wrapping quotes removed).
    configured: Option<PathBuf>,
    /// `<user home>\.claude`: Claude Code's home when the variable is not
    /// set at all.
    unconfigured: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ClaudeAdapter {
    executable: AgentExecutable,
    homes: ConfigHomes,
}

impl ClaudeAdapter {
    pub fn discover() -> Self {
        Self::discover_in(&DiscoveryContext::from_environment())
    }

    pub(crate) fn discover_in(context: &DiscoveryContext) -> Self {
        Self {
            executable: BINARY.resolve(
                context,
                |wrapper, _| {
                    let packaged = npm_packaged_executable(wrapper.parent()?);
                    packaged.is_file().then(|| Launcher::direct(packaged))
                },
                default_launcher,
            ),
            homes: ConfigHomes {
                configured: context
                    .configured_agent_home(CONFIG_DIR_VARIABLE)
                    .map(|home| simplify_path(&home)),
                unconfigured: context
                    .unconfigured_agent_home(".claude")
                    .map(|home| simplify_path(&home)),
            },
        }
    }

    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: AgentExecutable::direct(executable),
            homes: ConfigHomes::default(),
        }
    }

    /// Sets the home Claude Code uses without `CLAUDE_CONFIG_DIR`.
    #[cfg(test)]
    #[must_use]
    fn with_default_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.homes.unconfigured = Some(home.into());
        self
    }

    /// Every Claude Code command starts here. Claude Code must see exactly
    /// the variable that selects `agent_home`:
    /// * the environment's own `CLAUDE_CONFIG_DIR` home gets that value;
    /// * `<user home>\.claude` gets no variable at all, even when one is
    ///   inherited: setting it to the default path is not the same as
    ///   leaving it unset (Claude Code then looks for its global
    ///   `.claude.json` inside that directory and misses the sign-in), and an
    ///   inherited value (relative, quoted, or naming another directory)
    ///   would send Claude Code somewhere else;
    /// * any other home (a `ContextWake`-managed one) is passed explicitly.
    ///
    /// Without a home (version probes) the environment is inherited.
    fn base_command(&self, agent_home: Option<&Path>) -> Result<Command> {
        let mut command = self.executable.command()?;
        if let Some(home) = agent_home {
            let configured = self.homes.configured.as_deref();
            if let Some(configured) = configured.filter(|configured| same_home(home, configured)) {
                command.env(CONFIG_DIR_VARIABLE, configured);
            } else if self
                .homes
                .unconfigured
                .as_deref()
                .is_some_and(|unconfigured| same_home(home, unconfigured))
            {
                command.env_remove(CONFIG_DIR_VARIABLE);
            } else {
                command.env(CONFIG_DIR_VARIABLE, home);
            }
        }
        Ok(command)
    }

    /// `claude --resume <id>` in `workspace`, interactive.
    fn resume_command(
        &self,
        agent_home: &Path,
        workspace: &Path,
        session_id: &str,
    ) -> Result<Command> {
        let session_id = validate_session_reference(session_id)?;
        let workspace = conventional_path(workspace);
        let mut command = self.base_command(Some(agent_home))?;
        command
            .current_dir(workspace)
            .arg("--resume")
            .arg(session_id);
        inherit_stdio(&mut command);
        Ok(command)
    }

    /// New session: `claude [--model m] [--session-id <uuid>] --add-dir <H> -- <P>`.
    /// Resume: `claude --resume <id> [--model m] --add-dir <H> -- <P>`.
    ///
    /// `--add-dir` is variadic, so the options always end with `--`;
    /// otherwise the prompt would be taken as another directory and Claude
    /// Code would start without it. Every input is validated before a
    /// command exists.
    fn handoff_command(
        &self,
        agent_home: &Path,
        workspace: &Path,
        launch: &HandoffLaunch,
    ) -> Result<Command> {
        if launch.prompt.trim().is_empty() {
            return Err(ContextWakeError::InvalidData(
                "the handoff prompt for Claude Code is empty".into(),
            ));
        }
        if !launch.handoff_directory.join("context.md").is_file() {
            return Err(ContextWakeError::InvalidData(
                "handoff context.md is unavailable".into(),
            ));
        }
        let resume = launch
            .resume_session_id
            .as_deref()
            .map(validate_session_reference)
            .transpose()?;
        let model = launch
            .model
            .as_deref()
            .map(validate_external_reference)
            .transpose()?;
        let new_session_id = match (&resume, launch.new_session_id.as_deref()) {
            (None, Some(id)) => Some(
                Uuid::try_parse(id.trim())
                    .map_err(|_| {
                        ContextWakeError::InvalidData(
                            "a new Claude Code session ID must be a UUID".into(),
                        )
                    })?
                    .hyphenated()
                    .to_string(),
            ),
            _ => None,
        };
        let handoff_directory = conventional_path(&launch.handoff_directory);
        let workspace = conventional_path(workspace);

        let mut command = self.base_command(Some(agent_home))?;
        command.current_dir(workspace);
        if let Some(session_id) = &resume {
            command.arg("--resume").arg(session_id);
        }
        if let Some(model) = &model {
            command.arg("--model").arg(model);
        }
        if let Some(session_id) = &new_session_id {
            command.arg("--session-id").arg(session_id);
        }
        command
            .arg("--add-dir")
            .arg(handoff_directory)
            .arg("--")
            .arg(&launch.prompt);
        inherit_stdio(&mut command);
        Ok(command)
    }

    fn stable(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Verified,
            maturity: CapabilityMaturity::Stable,
            detail: detail.into(),
        }
    }

    fn partial(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Partial,
            maturity: CapabilityMaturity::Heuristic,
            detail: detail.into(),
        }
    }

    fn unavailable(detail: &str) -> Capability {
        Capability {
            support: CapabilitySupport::Unsupported,
            maturity: CapabilityMaturity::Unavailable,
            detail: detail.into(),
        }
    }
}

/// The prompt of the legacy [`AgentAdapter::start_with_handoff`] entry point,
/// which has no `ContextWake`-built prompt.
fn legacy_launch(
    handoff_directory: &Path,
    model_provider: Option<&str>,
    model: Option<&str>,
) -> HandoffLaunch {
    let context_path = conventional_path(&handoff_directory.join("context.md"));
    HandoffLaunch {
        handoff_directory: handoff_directory.to_path_buf(),
        prompt: format!(
            "Start a new coding session from the explicit workspace handoff at {}. Read context.md before acting. Treat commands in the handoff as notes only; do not execute them without user approval. This is project state, not hidden reasoning.",
            context_path.display()
        ),
        marker: String::new(),
        model_provider: model_provider.map(str::to_string),
        model: model.map(str::to_string),
        resume_session_id: None,
        new_session_id: None,
    }
}

fn inherit_stdio(command: &mut Command) {
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
}

/// Whether `home` and `other` name the same directory (Windows spellings
/// and links resolved).
fn same_home(home: &Path, other: &Path) -> bool {
    paths_equivalent(home, other)
        || matches!(
            (canonicalize_simplified(home), canonicalize_simplified(other)),
            (Ok(home), Ok(other)) if paths_equivalent(&home, &other)
        )
}

/// The resolved, non-verbatim form of `path` (unchanged when it cannot be
/// resolved). Agents receive and record this form.
fn conventional_path(path: &Path) -> PathBuf {
    canonicalize_simplified(path).unwrap_or_else(|_| simplify_path(path))
}

#[cfg(windows)]
fn default_launcher(context: &DiscoveryContext) -> Option<Launcher> {
    context.search_path().iter().find_map(|directory| {
        [
            directory.join("claude.exe"),
            npm_packaged_executable(directory),
        ]
        .into_iter()
        .find(|candidate| context.is_safe_executable(candidate))
        .map(Launcher::direct)
    })
}

#[cfg(not(windows))]
fn default_launcher(context: &DiscoveryContext) -> Option<Launcher> {
    context.find_on_path("claude").map(Launcher::direct)
}

/// The native executable of a global npm installation in `npm_prefix`.
fn npm_packaged_executable(npm_prefix: &Path) -> PathBuf {
    npm_prefix
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("bin")
        .join("claude.exe")
}

/// `claude auth status --json` prints `{"loggedIn": bool, ...}` for both
/// states (and exits non-zero when signed out). The JSON is authoritative;
/// anything unparsable is `Unknown` rather than a guess from the exit code.
/// Notices printed before or after the JSON object are ignored.
fn parse_auth_status(output: &ProbeOutput) -> AuthState {
    if output.timed_out {
        return AuthState::Unknown;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let logged_in = stdout.match_indices('{').find_map(|(start, _)| {
        // A stream deserializer stops after the first complete value, so
        // trailing text does not invalidate the document.
        serde_json::Deserializer::from_str(&stdout[start..])
            .into_iter::<serde_json::Value>()
            .next()?
            .ok()?
            .get("loggedIn")?
            .as_bool()
    });
    match logged_in {
        Some(true) => AuthState::SignedIn,
        Some(false) => AuthState::SignedOut,
        None => AuthState::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Local transcripts
// ---------------------------------------------------------------------------

/// Claude Code's project directory name for a working directory: every
/// character other than an ASCII letter or digit becomes `-`
/// (`C:\Users\Mike\x` → `C--Users-Mike-x`). The encoding is lossy, so a
/// directory found this way is confirmed with the transcripts' `cwd`.
fn encode_project_directory(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

/// The project directories in which Claude Code may have recorded sessions
/// started in `workspace`: the encoded resolved path and the encoded path as
/// given (they differ behind junctions, `subst` drives or 8.3 names).
fn project_directories(agent_home: &Path, workspace: &Path) -> Vec<PathBuf> {
    let root = agent_home.join(PROJECTS_DIRECTORY);
    if !is_real_directory(&root) {
        return Vec::new();
    }
    let mut names = Vec::<String>::new();
    for form in [
        canonicalize_simplified(workspace).ok(),
        Some(simplify_path(workspace)),
    ]
    .into_iter()
    .flatten()
    {
        let name = encode_project_directory(&form);
        if !name.is_empty() && !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    let mut directories = Vec::<PathBuf>::new();
    let add = |directory: PathBuf, directories: &mut Vec<PathBuf>| {
        if is_real_directory(&directory)
            && !directories
                .iter()
                .any(|known| paths_equivalent(known, &directory))
        {
            directories.push(directory);
        }
    };
    for name in &names {
        add(root.join(name), &mut directories);
    }
    let prefixes = names
        .iter()
        .filter(|name| name.len() > MAX_ENCODED_NAME)
        .map(|name| &name[..MAX_ENCODED_NAME])
        .collect::<Vec<_>>();
    if !prefixes.is_empty()
        && let Ok(entries) = std::fs::read_dir(&root)
    {
        for entry in entries.take(MAX_DIRECTORY_ENTRIES).flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|prefix| {
                name.get(..MAX_ENCODED_NAME)
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            }) {
                add(entry.path(), &mut directories);
            }
        }
    }
    directories
}

/// A transcript file `<session-id>.jsonl`.
#[derive(Clone, Debug)]
struct TranscriptFile {
    session_id: String,
    path: PathBuf,
    modified: Option<SystemTime>,
}

impl TranscriptFile {
    fn at(directory: &Path, file_name: &str) -> Option<Self> {
        let stem = file_name
            .strip_suffix(".jsonl")
            .or_else(|| file_name.strip_suffix(".JSONL"))?;
        let session_id = validate_session_reference(stem).ok()?;
        // Session references may contain ':', which Windows reads as a drive
        // prefix (`C:x.jsonl` replaces the directory in `join`) or an
        // alternate data stream (`x.jsonl:s`); a transcript is always one
        // plain file name directly inside `directory`.
        if session_id != stem || file_name.contains(':') {
            return None;
        }
        let path = directory.join(file_name);
        let mut components = Path::new(file_name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) || path.parent() != Some(directory)
        {
            return None;
        }
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        Some(Self {
            session_id,
            path,
            modified: metadata.modified().ok(),
        })
    }
}

/// Transcripts in `directories`, newest first by modification time.
fn transcript_files(directories: &[PathBuf]) -> Vec<TranscriptFile> {
    let mut files = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.take(MAX_DIRECTORY_ENTRIES).flatten() {
            let name = entry.file_name();
            if let Some(file) = name
                .to_str()
                .and_then(|name| TranscriptFile::at(directory, name))
            {
                files.push(file);
            }
        }
    }
    files.sort_by_key(|file| Reverse(file.modified));
    files.truncate(MAX_TRANSCRIPT_FILES);
    files
}

/// What one transcript line contributes.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// A typed user message, as its text blocks.
    User(Vec<String>),
    /// Visible text of an assistant message (no thinking or tool use).
    Assistant {
        message_id: Option<String>,
        text: String,
    },
    /// A title the user gave the session (`/rename`).
    CustomTitle(String),
    /// A generated conversation summary.
    Summary(String),
    Other,
}

#[derive(Debug)]
struct Record {
    timestamp: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    entry: Entry,
}

fn flag(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool) == Some(true)
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

/// Text blocks of a typed user message. Meta, sidechain (subagent),
/// compaction-summary and transcript-only records, and tool results are not
/// typed requests.
fn typed_user_blocks(object: &Map<String, Value>) -> Option<Vec<String>> {
    if [
        "isMeta",
        "isSidechain",
        "isCompactSummary",
        "isVisibleInTranscriptOnly",
    ]
    .iter()
    .any(|key| flag(object, key))
        || object.contains_key("toolUseResult")
    {
        return None;
    }
    let message = object.get("message")?;
    if message
        .get("role")
        .and_then(Value::as_str)
        .is_some_and(|role| role != "user")
    {
        return None;
    }
    let blocks = match message.get("content")? {
        Value::String(text) => vec![text.clone()],
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|block| block_type(block) == Some("tool_result"))
            {
                return None;
            }
            blocks
                .iter()
                .filter(|block| block_type(block) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        }
        _ => return None,
    };
    let blocks = blocks
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!blocks.is_empty()).then_some(blocks)
}

/// Visible text of an assistant message; synthetic and API-error messages
/// written by Claude Code itself are not answers.
fn assistant_text(object: &Map<String, Value>) -> Option<(Option<String>, String)> {
    if ["isSidechain", "isMeta", "isApiErrorMessage"]
        .iter()
        .any(|key| flag(object, key))
    {
        return None;
    }
    let message = object.get("message")?;
    if message.get("model").and_then(Value::as_str) == Some("<synthetic>") {
        return None;
    }
    let text = match message.get("content")? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block_type(block) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    (!text.trim().is_empty()).then_some((message_id, text))
}

fn parse_record(line: &[u8]) -> Option<Record> {
    let line = line.trim_ascii();
    if line.first() != Some(&b'{') {
        return None;
    }
    let value = serde_json::from_slice::<Value>(line).ok()?;
    let object = value.as_object()?;
    let text_field = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    let entry = match object.get("type").and_then(Value::as_str) {
        Some("user") => typed_user_blocks(object).map_or(Entry::Other, Entry::User),
        Some("assistant") => assistant_text(object).map_or(Entry::Other, |(message_id, text)| {
            Entry::Assistant { message_id, text }
        }),
        Some("custom-title") => text_field("customTitle").map_or(Entry::Other, Entry::CustomTitle),
        Some("summary") => text_field("summary").map_or(Entry::Other, Entry::Summary),
        _ => Entry::Other,
    };
    Some(Record {
        timestamp: object
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        cwd: text_field("cwd").map(PathBuf::from),
        entry,
    })
}

fn is_handoff_prompt(text: &str) -> bool {
    text.trim_start().starts_with(HANDOFF_PROMPT_PREFIX)
}

fn is_injected_text(text: &str) -> bool {
    let text = text.trim_start();
    INJECTED_USER_TEXT
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// The request a typed user message represents in an excerpt or title:
/// `ContextWake`'s own handoff prompt is summarized, injected blocks are
/// dropped.
fn user_request(blocks: &[String]) -> Option<String> {
    if blocks.iter().any(|text| is_handoff_prompt(text)) {
        return Some(DELIVERED_HANDOFF_REQUEST.into());
    }
    let text = blocks
        .iter()
        .filter(|text| !is_injected_text(text))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Complete lines of `bytes`; a trailing line without a line break is
/// dropped when `cut` is set (the read stopped inside it).
fn lines(bytes: &[u8], cut: bool) -> impl Iterator<Item = &[u8]> {
    let end = if cut {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1)
    } else {
        bytes.len()
    };
    bytes[..end].split(|byte| *byte == b'\n')
}

/// Metadata collected from the head and tail of a transcript.
#[derive(Clone, Debug, Default)]
struct TranscriptSummary {
    cwd: Option<PathBuf>,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    custom_title: Option<String>,
    summary: Option<String>,
    first_request: Option<String>,
}

impl TranscriptSummary {
    fn observe(&mut self, record: Record) {
        if self.cwd.is_none() {
            self.cwd = record.cwd;
        }
        if let Some(timestamp) = record.timestamp {
            self.first_timestamp.get_or_insert(timestamp);
            self.last_timestamp = self.last_timestamp.max(Some(timestamp));
        }
        match record.entry {
            Entry::CustomTitle(title) => self.custom_title = Some(title),
            Entry::Summary(summary) => self.summary = Some(summary),
            Entry::User(blocks) if self.first_request.is_none() => {
                // ContextWake's own prompt is not a title.
                self.first_request =
                    user_request(&blocks).filter(|request| request != DELIVERED_HANDOFF_REQUEST);
            }
            _ => {}
        }
    }

    /// The user's title, else Claude Code's summary, else the first request.
    fn title(&self) -> Option<String> {
        [&self.custom_title, &self.summary, &self.first_request]
            .into_iter()
            .flatten()
            .find_map(|title| safe_agent_label(title))
            .map(|title| title.chars().take(200).collect())
    }
}

fn summarize_transcript(path: &Path) -> Option<TranscriptSummary> {
    let head = read_file_prefix(path, LISTING_HEAD_BYTES).ok()?;
    let complete = head.len() < LISTING_HEAD_BYTES;
    let mut summary = TranscriptSummary::default();
    for record in lines(&head, !complete).filter_map(parse_record) {
        summary.observe(record);
    }
    if !complete && let Ok((tail, _)) = read_file_tail(path, LISTING_TAIL_BYTES) {
        for record in lines(&tail, false).filter_map(parse_record) {
            summary.observe(record);
        }
    }
    Some(summary)
}

/// A transcript confirmed to belong to the workspace.
#[derive(Clone, Debug)]
struct WorkspaceSession {
    file: TranscriptFile,
    summary: TranscriptSummary,
}

impl WorkspaceSession {
    fn modified(&self) -> Option<DateTime<Utc>> {
        self.file.modified.map(DateTime::<Utc>::from)
    }

    fn updated_at(&self) -> Option<DateTime<Utc>> {
        self.summary.last_timestamp.or_else(|| self.modified())
    }

    fn discovered(&self) -> DiscoveredAgentSession {
        DiscoveredAgentSession {
            provider_session_id: self.file.session_id.clone(),
            title: self.summary.title(),
            workspace_path: self.summary.cwd.as_deref().map(simplify_path),
            created_at: self.summary.first_timestamp.or_else(|| self.modified()),
            updated_at: self.updated_at(),
        }
    }

    fn launched(&self, prompt_delivered: bool) -> LaunchedSession {
        LaunchedSession {
            provider_session_id: self.file.session_id.clone(),
            title: self.summary.title(),
            last_activity_at: self.updated_at(),
            prompt_delivered,
        }
    }
}

fn confirm(file: TranscriptFile, matcher: &mut WorkspaceMatcher) -> Option<WorkspaceSession> {
    let summary = summarize_transcript(&file.path)?;
    summary
        .cwd
        .as_deref()
        .is_some_and(|cwd| matcher.matches(cwd))
        .then_some(WorkspaceSession { file, summary })
}

/// Up to `limit` sessions of `workspace`, newest first by modification,
/// optionally only those modified at or after `modified_since`.
fn workspace_sessions(
    agent_home: &Path,
    workspace: &Path,
    limit: usize,
    modified_since: Option<SystemTime>,
) -> Vec<WorkspaceSession> {
    let mut matcher = WorkspaceMatcher::new(workspace);
    transcript_files(&project_directories(agent_home, workspace))
        .into_iter()
        .filter(|file| {
            modified_since.is_none_or(|since| file.modified.is_some_and(|time| time >= since))
        })
        .filter_map(|file| confirm(file, &mut matcher))
        .take(limit)
        .collect()
}

/// The transcript of `session_id` in `workspace`, if it is there.
fn workspace_session(
    agent_home: &Path,
    workspace: &Path,
    session_id: &str,
) -> Option<WorkspaceSession> {
    let mut matcher = WorkspaceMatcher::new(workspace);
    let file_name = format!("{session_id}.jsonl");
    project_directories(agent_home, workspace)
        .iter()
        .filter_map(|directory| TranscriptFile::at(directory, &file_name))
        .find_map(|file| confirm(file, &mut matcher))
}

/// User requests and the latest answer in transcript text.
fn transcript_messages(bytes: &[u8], partial: bool) -> TranscriptMessages {
    let mut messages = TranscriptMessages {
        partial,
        ..TranscriptMessages::default()
    };
    // Claude Code writes each content block of an API message as its own
    // record; text blocks of the same message belong to one answer.
    let mut answer_id = None::<String>;
    for record in lines(bytes, false).filter_map(parse_record) {
        match record.entry {
            Entry::User(blocks) => {
                if let Some(request) = user_request(&blocks) {
                    messages.user_requests.push(request);
                }
            }
            Entry::Assistant { message_id, text } => match &mut messages.last_agent_message {
                Some(answer) if message_id.is_some() && message_id == answer_id => {
                    answer.push('\n');
                    answer.push_str(&text);
                }
                _ => {
                    messages.last_agent_message = Some(text);
                    answer_id = message_id;
                }
            },
            _ => {}
        }
    }
    messages
}

/// The longest part of `marker` that every JSON encoder writes literally
/// (printable ASCII other than quotes, backslashes and HTML-sensitive
/// characters), used to skip lines cheaply before parsing them.
fn literal_marker_fragment(marker: &str) -> Option<&str> {
    marker
        .split(|character: char| {
            !(character == ' ' || character.is_ascii_graphic())
                || matches!(character, '"' | '\\' | '<' | '>' | '&' | '\'')
        })
        .max_by_key(|fragment| fragment.len())
        .filter(|fragment| fragment.len() >= 8)
}

/// Reads one line into `line`, keeping at most `max_bytes` of it. Returns
/// `None` at the end of the input, otherwise whether the line was kept whole.
fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut read_any = false;
    let mut whole = true;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(read_any.then_some(whole));
        }
        read_any = true;
        let (used, finished) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        if whole && line.len() + used <= max_bytes {
            line.extend_from_slice(&available[..used]);
        } else {
            whole = false;
            line.clear();
        }
        reader.consume(used);
        if finished {
            return Ok(Some(whole));
        }
    }
}

/// Whether a typed user message written at or after `not_before` contains
/// `marker`. Only the last [`DELIVERY_SCAN_BYTES`] of the file are searched.
fn transcript_contains_marker(path: &Path, marker: &str, not_before: DateTime<Utc>) -> bool {
    if marker.trim().is_empty() {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return false;
    };
    let mut skip_partial_line = false;
    if length > DELIVERY_SCAN_BYTES {
        if file
            .seek(SeekFrom::Start(length - DELIVERY_SCAN_BYTES))
            .is_err()
        {
            return false;
        }
        skip_partial_line = true;
    }
    let fragment = literal_marker_fragment(marker).map(str::as_bytes);
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    while let Ok(Some(whole)) = read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES) {
        if std::mem::take(&mut skip_partial_line) || !whole {
            continue;
        }
        if fragment.is_some_and(|fragment| {
            !line
                .windows(fragment.len())
                .any(|window| window == fragment)
        }) {
            continue;
        }
        if let Some(Record {
            timestamp: Some(timestamp),
            entry: Entry::User(blocks),
            ..
        }) = parse_record(&line)
            && timestamp >= not_before
            && blocks.iter().any(|text| text.contains(marker))
        {
            return true;
        }
    }
    false
}

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["claude-code"]
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn model_providers(&self) -> Vec<ModelProvider> {
        vec![
            ModelProvider {
                id: "anthropic".into(),
                display_name: "Anthropic".into(),
                local: false,
            },
            ModelProvider {
                id: "amazon-bedrock".into(),
                display_name: "Amazon Bedrock".into(),
                local: false,
            },
            ModelProvider {
                id: "google-vertex".into(),
                display_name: "Google Vertex AI".into(),
                local: false,
            },
            ModelProvider {
                id: "microsoft-foundry".into(),
                display_name: "Microsoft Foundry".into(),
                local: false,
            },
        ]
    }

    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            installation_detection: Self::stable("`claude --version`"),
            version_detection: Self::stable("`claude --version`"),
            auth_status: Self::stable("`claude auth status --json`"),
            login: Self::stable("provider-owned `claude auth login`"),
            logout: Self::stable("provider-owned `claude auth logout`"),
            multiple_profiles: Self::stable(
                "CLAUDE_CONFIG_DIR is documented for side-by-side accounts",
            ),
            native_resume: Self::stable(&format!(
                "same-profile `claude --resume <id> --add-dir <handoff> -- <prompt>` appends to the original transcript; verified with {VERIFIED_VERSION} on Windows, argv covered by contract tests"
            )),
            session_listing: Self::stable(&format!(
                "local transcripts `<config dir>\\projects\\<encoded cwd>\\<session id>.jsonl`, matched by their recorded cwd; format verified with {VERIFIED_VERSION}"
            )),
            named_sessions: Self::stable("sessions can be named and resumed by name"),
            context_reporting: Self::partial(
                "`/context` is available in-session; no external machine-readable probe",
            ),
            usage_reporting: Self::unavailable(
                "no stable account quota interface is used by ContextWake",
            ),
            model_reporting: Self::partial(
                "model is visible in-session but not queried by the P0 adapter",
            ),
            model_selection: Self::stable("`claude --model <model>`"),
            available_models: Self::partial(
                "the in-session model picker is authoritative; no P0 machine-readable list",
            ),
            multiple_model_providers: Self::stable(
                "Anthropic, Bedrock, Vertex AI, and Foundry modes are documented",
            ),
            programmatic_interface: Self::stable("print mode and JSON output are documented"),
            non_interactive_mode: Self::stable("`claude --print`"),
            structured_output: Self::stable("JSON and stream-JSON print output"),
            acp: Self::unavailable("no native Claude Code ACP server is used"),
            mcp: Self::stable("Claude Code supports configured MCP servers"),
            portable_handoff: Self::stable(&format!(
                "interactive launch with the handoff as the first message (`--add-dir <handoff> -- <prompt>`, optional `--session-id`); delivery is confirmed from the transcript; verified with {VERIFIED_VERSION}"
            )),
            cloud_handoff: Self::unavailable(
                "ContextWake does not initiate remote Claude tasks from the local adapter",
            ),
            local_models: Self::unavailable("no supported local-model backend is documented"),
            profile_isolation: Self::partial(
                "CLAUDE_CONFIG_DIR isolates settings, credentials, sessions, and plugins; macOS keychain behavior still requires platform QA",
            ),
        }
    }

    fn detect(&self, agent_home: Option<&Path>) -> Result<AgentHealth> {
        if let Some(health) = self.executable.unavailable_health(self.id()) {
            return Ok(health);
        }
        let mut command = self.base_command(agent_home)?;
        command.arg("--version");
        match run_probe(command, PROBE_TIMEOUT, MAX_PROBE_OUTPUT) {
            Ok(output) if output.success => {
                let version = safe_agent_text(String::from_utf8_lossy(&output.stdout).trim());
                let mut message = String::from("Claude Code detected");
                let auth_state = advisory_auth_state(
                    agent_home.map(|home| self.auth_status(home)),
                    &mut message,
                );
                Ok(AgentHealth {
                    agent_id: self.id().into(),
                    installed: true,
                    executable: self.executable.reported_path(),
                    version: Some(version),
                    auth_state,
                    message,
                })
            }
            Ok(output) => Ok(AgentHealth {
                agent_id: self.id().into(),
                installed: false,
                executable: self.executable.reported_path(),
                version: None,
                auth_state: AuthState::Unknown,
                message: if output.timed_out {
                    format!(
                        "Claude Code version probe timed out after {} seconds",
                        PROBE_TIMEOUT.as_secs()
                    )
                } else {
                    safe_probe_diagnostic(&output)
                },
            }),
            Err(error) => Ok(AgentHealth {
                agent_id: self.id().into(),
                installed: false,
                executable: None,
                version: None,
                auth_state: AuthState::Unknown,
                message: format!(
                    "Claude Code could not be found ({error}). Install Claude Code or set CONTEXTWAKE_CLAUDE_BIN."
                ),
            }),
        }
    }

    fn auth_status(&self, agent_home: &Path) -> Result<AuthState> {
        let mut command = self.base_command(Some(agent_home))?;
        command.args(["auth", "status", "--json"]);
        let output = run_probe(command, PROBE_TIMEOUT, MAX_PROBE_OUTPUT).map_err(|error| {
            ContextWakeError::Provider(format!("could not query Claude Code auth: {error}"))
        })?;
        Ok(parse_auth_status(&output))
    }

    fn initialize_profile_home(&self, agent_home: &Path) -> Result<()> {
        std::fs::create_dir_all(agent_home).at(agent_home)?;
        let metadata = std::fs::symlink_metadata(agent_home).at(agent_home)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ContextWakeError::UnsafePath(format!(
                "agent home must be a real directory: {}",
                agent_home.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(agent_home, std::fs::Permissions::from_mode(0o700))
                .at(agent_home)?;
        }
        Ok(())
    }

    fn login(&self, agent_home: &Path, device_auth: bool) -> Result<AuthState> {
        if device_auth {
            return Err(ContextWakeError::CapabilityUnavailable(
                "Claude Code does not expose the Codex --device-auth flag; run without --device-auth"
                    .into(),
            ));
        }
        let status = self
            .base_command(Some(agent_home))?
            .args(["auth", "login"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Claude Code login: {error}"))
            })?;
        if status.success() {
            Ok(AuthState::SignedIn)
        } else {
            Err(ContextWakeError::Provider(format!(
                "Claude Code login exited with status {status}; the previous ContextWake profile remains unchanged"
            )))
        }
    }

    fn logout(&self, agent_home: &Path) -> Result<()> {
        let status = self
            .base_command(Some(agent_home))?
            .args(["auth", "logout"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Claude Code logout: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(ContextWakeError::Provider(format!(
                "Claude Code logout exited with status {status}"
            )))
        }
    }

    fn list_sessions(
        &self,
        agent_home: &Path,
        workspace: &Path,
        max_count: usize,
    ) -> Result<Vec<DiscoveredAgentSession>> {
        let limit = max_count.min(MAX_TRANSCRIPT_FILES);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut sessions = workspace_sessions(agent_home, workspace, limit, None)
            .iter()
            .map(WorkspaceSession::discovered)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| Reverse(session.updated_at.or(session.created_at)));
        Ok(sessions)
    }

    fn resume(&self, agent_home: &Path, workspace: &Path, session_id: &str) -> Result<()> {
        let status = self
            .resume_command(agent_home, workspace, session_id)?
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Claude Code resume: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(ContextWakeError::Provider(format!(
                "Claude Code could not natively resume this session (status {status}). Create a workspace handoff instead."
            )))
        }
    }

    fn start_with_handoff(
        &self,
        agent_home: &Path,
        workspace: &Path,
        handoff_directory: &Path,
        model_provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<()> {
        self.launch_with_handoff(
            agent_home,
            workspace,
            &legacy_launch(handoff_directory, model_provider, model),
        )
    }

    fn default_home(&self) -> Option<PathBuf> {
        self.homes
            .configured
            .clone()
            .or_else(|| self.homes.unconfigured.clone())
    }

    fn conversation_excerpt(
        &self,
        agent_home: &Path,
        workspace: &Path,
        provider_session_id: Option<&str>,
    ) -> Result<Option<ConversationExcerpt>> {
        let session = match provider_session_id {
            Some(id) => workspace_session(agent_home, workspace, &validate_session_reference(id)?),
            None => workspace_sessions(agent_home, workspace, 1, None)
                .into_iter()
                .next(),
        };
        let Some(session) = session else {
            return Ok(None);
        };
        let (tail, partial) = read_file_tail(&session.file.path, EXCERPT_TAIL_BYTES)?;
        Ok(Some(conversation_excerpt_from(
            self.id(),
            &session.file.session_id,
            session.summary.title().as_deref(),
            session.updated_at(),
            &transcript_messages(&tail, partial),
        )))
    }

    fn launch_with_handoff(
        &self,
        agent_home: &Path,
        workspace: &Path,
        launch: &HandoffLaunch,
    ) -> Result<()> {
        let resumed = launch.resume_session_id.is_some();
        let status = self
            .handoff_command(agent_home, workspace, launch)?
            .status()
            .map_err(|error| {
                ContextWakeError::Provider(format!("could not start Claude Code: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else if resumed {
            Err(ContextWakeError::Provider(format!(
                "Claude Code exited with status {status} while resuming the session with the handoff; delivery is not confirmed"
            )))
        } else {
            Err(ContextWakeError::Provider(format!(
                "Claude Code new-session launch exited with status {status}; no native resume was claimed"
            )))
        }
    }

    /// The expected session (pre-assigned with `--session-id` or resumed) is
    /// checked first; otherwise, or when the prompt is not in it, the
    /// workspace's transcripts modified since the launch are searched for the
    /// marker. Without a marker match the expected session is still reported
    /// (undelivered); without either, nothing is guessed.
    fn launched_session(
        &self,
        agent_home: &Path,
        workspace: &Path,
        record: &LaunchRecord,
    ) -> Result<Option<LaunchedSession>> {
        let not_before = record.started_at - chrono::Duration::seconds(CLOCK_SKEW_SECONDS);
        let expected = record
            .expected_session_id
            .as_deref()
            .map(validate_session_reference)
            .transpose()?
            .and_then(|id| workspace_session(agent_home, workspace, &id));
        if let Some(session) = &expected
            && transcript_contains_marker(&session.file.path, &record.marker, not_before)
        {
            return Ok(Some(session.launched(true)));
        }
        let delivered = workspace_sessions(
            agent_home,
            workspace,
            MAX_LAUNCH_CANDIDATES,
            Some(SystemTime::from(not_before)),
        )
        .into_iter()
        .filter(|session| {
            expected
                .as_ref()
                .is_none_or(|known| known.file.session_id != session.file.session_id)
        })
        .find(|session| transcript_contains_marker(&session.file.path, &record.marker, not_before));
        Ok(delivered
            .map(|session| session.launched(true))
            .or_else(|| expected.map(|session| session.launched(false))))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::provider::common::tests::fake_agent;
    use crate::provider::discovery::commands_built_on_this_thread;
    use crate::provider::discovery::tests::executable;
    use serde_json::json;

    #[test]
    fn capabilities_are_agent_and_backend_aware() {
        let capabilities = ClaudeAdapter::discover().capabilities();
        assert!(capabilities.native_resume.is_available());
        assert!(capabilities.model_selection.is_available());
        assert!(capabilities.multiple_model_providers.is_available());
        assert!(!capabilities.local_models.is_available());
    }

    #[test]
    fn profile_home_contains_no_credentials() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path().join("profile");
        ClaudeAdapter::with_executable("unused")
            .initialize_profile_home(&home)
            .expect("profile home");
        assert!(home.is_dir());
        assert!(
            std::fs::read_dir(home)
                .expect("read profile home")
                .next()
                .is_none()
        );
    }

    #[test]
    fn missing_executable_is_reported_without_crashing() {
        let health = ClaudeAdapter::with_executable("contextwake-test-missing-claude")
            .detect(None)
            .expect("health result");
        assert!(!health.installed);
        assert_eq!(health.auth_state, AuthState::Unknown);
        assert!(health.message.contains("could not be found"));
    }

    fn probe(success: bool, stdout: &str) -> ProbeOutput {
        ProbeOutput {
            success,
            stdout: stdout.as_bytes().to_vec(),
            ..ProbeOutput::default()
        }
    }

    #[test]
    fn auth_status_json_is_authoritative() {
        // Shapes captured from Claude Code 2.1.319 (identity fields omitted).
        let signed_in =
            r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty"}"#;
        let signed_out = r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty", "loginExpiresAt": null}"#;
        assert_eq!(
            parse_auth_status(&probe(true, signed_in)),
            AuthState::SignedIn
        );
        assert_eq!(
            parse_auth_status(&probe(false, signed_out)),
            AuthState::SignedOut
        );
        // The exit status alone used to decide: success meant signed in.
        assert_eq!(
            parse_auth_status(&probe(true, signed_out)),
            AuthState::SignedOut
        );
        assert_eq!(
            parse_auth_status(&probe(true, &format!("Update available\n{signed_in}\n"))),
            AuthState::SignedIn
        );
        // Text after the object, or a brace inside a leading notice, does
        // not hide the document.
        assert_eq!(
            parse_auth_status(&probe(true, &format!("{signed_in}\nUpdate available\n"))),
            AuthState::SignedIn
        );
        assert_eq!(
            parse_auth_status(&probe(
                false,
                &format!("Notice {{see docs}}\n{signed_out}\nRun `claude login`\n")
            )),
            AuthState::SignedOut
        );
        assert_eq!(
            parse_auth_status(&probe(true, "Logged in")),
            AuthState::Unknown
        );
        assert_eq!(
            parse_auth_status(&probe(false, "error: unknown option '--json'")),
            AuthState::Unknown
        );
        let timed_out = ProbeOutput {
            timed_out: true,
            ..probe(false, signed_in)
        };
        assert_eq!(parse_auth_status(&timed_out), AuthState::Unknown);
    }

    #[test]
    fn auth_probe_reads_logged_in_from_the_agent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let adapter = ClaudeAdapter {
            executable: AgentExecutable::Available(fake_agent(
                directory.path(),
                r#"echo {"loggedIn": false, "authMethod": "none"}"#,
                r#"printf '%s\n' '{"loggedIn": false, "authMethod": "none"}'"#,
            )),
            homes: ConfigHomes::default(),
        };
        assert_eq!(
            adapter.auth_status(directory.path()).expect("auth"),
            AuthState::SignedOut
        );
    }

    #[test]
    fn native_installer_location_is_found_from_the_home_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let bin = home.join(".local").join("bin");
        let name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let claude = executable(&bin.join(name));
        let context = DiscoveryContext::new(Some(&home), Some(&home), [bin]);
        assert_eq!(
            ClaudeAdapter::discover_in(&context).executable,
            AgentExecutable::direct(claude)
        );
    }

    #[test]
    fn continuity_capabilities_name_the_verified_version() {
        let capabilities = ClaudeAdapter::with_executable("unused").capabilities();
        for capability in [
            &capabilities.native_resume,
            &capabilities.session_listing,
            &capabilities.portable_handoff,
        ] {
            assert_eq!(capability.support, CapabilitySupport::Verified);
            assert!(
                capability.detail.contains("2.1.319"),
                "{}",
                capability.detail
            );
        }
        // Catalog use is gated on `Verified`; the in-session picker stays
        // descriptive.
        assert_ne!(
            capabilities.available_models.support,
            CapabilitySupport::Verified
        );
    }

    // -- Homes and environment ---------------------------------------------

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Users\Test User")
        } else {
            PathBuf::from("/home/test user")
        }
    }

    #[test]
    fn default_home_is_the_config_variable_or_the_user_home() {
        let home = root();
        let context = DiscoveryContext::new(Some(&home), Some(&home), Vec::new());
        assert_eq!(
            ClaudeAdapter::discover_in(&context).default_home(),
            Some(home.join(".claude"))
        );
        let configured = home.join("Claude Config");
        let overridden = context
            .clone()
            .with_override(CONFIG_DIR_VARIABLE, configured.as_os_str());
        assert_eq!(
            ClaudeAdapter::discover_in(&overridden).default_home(),
            Some(configured)
        );
        // A relative value is not a home Claude Code would use reliably.
        let relative = context.with_override(CONFIG_DIR_VARIABLE, "relative");
        assert_eq!(
            ClaudeAdapter::discover_in(&relative).default_home(),
            Some(home.join(".claude"))
        );
        let without_home = DiscoveryContext::new(None, None, Vec::new());
        assert_eq!(
            ClaudeAdapter::discover_in(&without_home).default_home(),
            None
        );
        assert_eq!(
            ClaudeAdapter::with_executable("claude").default_home(),
            None
        );
    }

    /// How [`config_dir`] reports a removed variable.
    const REMOVED: &str = "<removed>";

    /// The `CLAUDE_CONFIG_DIR` the command sets (`None` when it inherits the
    /// variable unchanged). Removing the variable shows as [`REMOVED`].
    fn config_dir(command: &Command) -> Option<String> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(CONFIG_DIR_VARIABLE))
            .map(|(_, value)| {
                value.map_or_else(
                    || REMOVED.into(),
                    |value| value.to_string_lossy().into_owned(),
                )
            })
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn an_inherited_config_dir_never_redirects_the_selected_home() {
        // The homes as discovered, with an executable for building commands.
        let discovered = |context: &DiscoveryContext| ClaudeAdapter {
            executable: AgentExecutable::direct("claude"),
            ..ClaudeAdapter::discover_in(context)
        };
        let home = root();
        let user_default = home.join(".claude");
        let isolated = home.join("ContextWake").join("profiles").join("work");
        let context = DiscoveryContext::new(Some(&home), Some(&home), Vec::new());
        let config_dir_for = |context: &DiscoveryContext, target: &Path| {
            config_dir(
                &discovered(context)
                    .base_command(Some(target))
                    .expect("command"),
            )
        };

        // A relative or empty inherited value is not Claude Code's home here,
        // so the user default home must not inherit it.
        for unusable in ["relative", ""] {
            let context = context.clone().with_override(CONFIG_DIR_VARIABLE, unusable);
            assert_eq!(
                config_dir_for(&context, &user_default),
                Some(REMOVED.into()),
                "{unusable:?}"
            );
        }

        // An existing-login profile stored the variable-less home and the
        // user later configured another directory: the profile keeps its
        // home without the variable; the configured home gets its value.
        let configured = home.join("Claude Config");
        let quoted = format!("\"{}\"", configured.display());
        let configured_context = context
            .clone()
            .with_override(CONFIG_DIR_VARIABLE, quoted.as_str());
        assert_eq!(
            ClaudeAdapter::discover_in(&configured_context).default_home(),
            Some(configured.clone())
        );
        assert_eq!(
            config_dir_for(&configured_context, &user_default),
            Some(REMOVED.into())
        );
        assert_eq!(
            config_dir_for(&configured_context, &configured),
            Some(text(&configured))
        );
        assert_eq!(
            config_dir_for(&configured_context, &isolated),
            Some(text(&isolated))
        );
        // Version probes without a home inherit the environment.
        assert_eq!(
            config_dir(
                &discovered(&configured_context)
                    .base_command(None)
                    .expect("command")
            ),
            None
        );
    }

    #[test]
    fn the_default_home_is_never_passed_as_config_dir() {
        let default = root().join(".claude");
        let adapter = ClaudeAdapter::with_executable("claude").with_default_home(&default);
        let command = adapter.base_command(Some(&default)).expect("command");
        assert_eq!(config_dir(&command), Some(REMOVED.into()));
        assert_eq!(
            config_dir(&adapter.base_command(None).expect("command")),
            None
        );

        let isolated = root().join("ContextWake").join("profiles").join("work");
        let command = adapter.base_command(Some(&isolated)).expect("command");
        assert_eq!(config_dir(&command), Some(text(&isolated)));
        // Without a known default home every home is passed explicitly.
        let command = ClaudeAdapter::with_executable("claude")
            .base_command(Some(&default))
            .expect("command");
        assert_eq!(config_dir(&command), Some(text(&default)));
    }

    #[cfg(windows)]
    #[test]
    fn default_home_comparison_follows_windows_path_rules() {
        let adapter = ClaudeAdapter::with_executable("claude")
            .with_default_home(r"C:\Users\Test User\.claude");
        for spelling in [
            r"c:\users\test user\.claude",
            r"C:/Users/Test User/.claude/",
            r"\\?\C:\Users\Test User\.claude",
        ] {
            let command = adapter
                .base_command(Some(Path::new(spelling)))
                .expect("command");
            assert_eq!(config_dir(&command), Some(REMOVED.into()), "{spelling}");
        }
    }

    #[test]
    fn every_command_applies_the_default_home_rule() {
        let fixture = LaunchFixture::new();
        let adapter = ClaudeAdapter::with_executable("claude").with_default_home(&fixture.home);
        let launch = fixture.launch(None, Some(SESSION_A));
        let commands = [
            adapter
                .base_command(Some(&fixture.home))
                .expect("detect/auth/login/logout"),
            adapter
                .resume_command(&fixture.home, &fixture.workspace, SESSION_A)
                .expect("resume"),
            adapter
                .handoff_command(&fixture.home, &fixture.workspace, &launch)
                .expect("launch"),
        ];
        for command in &commands {
            assert_eq!(config_dir(command), Some(REMOVED.into()));
        }
        let other = fixture.root.path().join("isolated home");
        let command = adapter
            .handoff_command(&other, &fixture.workspace, &launch)
            .expect("launch");
        assert_eq!(config_dir(&command), Some(text(&other)));
    }

    // -- Launch argv ---------------------------------------------------------

    const SESSION_A: &str = "0b6c8f9e-2d4a-4c1b-9a7e-3f5d6c7b8a91";
    const SESSION_B: &str = "5f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

    /// A workspace and handoff with spaces in their paths, as on this machine.
    struct LaunchFixture {
        root: tempfile::TempDir,
        home: PathBuf,
        workspace: PathBuf,
        handoff: PathBuf,
    }

    impl LaunchFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let home = root.path().join("Claude Home");
            let workspace = root.path().join("Work Files").join("my app");
            let handoff = root.path().join("Context Wake").join("handoff 1");
            for directory in [&home, &workspace, &handoff] {
                std::fs::create_dir_all(directory).expect("fixture directory");
            }
            std::fs::write(handoff.join("context.md"), "# Context").expect("context");
            Self {
                root,
                home,
                workspace,
                handoff,
            }
        }

        fn launch(&self, resume: Option<&str>, new_session: Option<&str>) -> HandoffLaunch {
            HandoffLaunch {
                handoff_directory: self.handoff.clone(),
                prompt: PROMPT.into(),
                marker: "ContextWake handoff 1234".into(),
                model_provider: Some("anthropic".into()),
                model: Some("claude-opus-4-1".into()),
                resume_session_id: resume.map(str::to_string),
                new_session_id: new_session.map(str::to_string),
            }
        }
    }

    /// One argument with line breaks, quotes and option-like text.
    const PROMPT: &str =
        "ContextWake handoff 1234 \u{b7} launch abcd\n\"quoted\" --add-dir x\n-- -p\r\nend";

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn display(path: &Path) -> String {
        conventional_path(path).to_string_lossy().into_owned()
    }

    #[test]
    fn new_session_launch_ends_options_before_the_prompt() {
        let fixture = LaunchFixture::new();
        let adapter = ClaudeAdapter::with_executable(r"C:\Program Files\Claude\claude.exe");
        let command = adapter
            .handoff_command(
                &fixture.home,
                &fixture.workspace,
                &fixture.launch(None, Some(&SESSION_A.to_uppercase())),
            )
            .expect("command");
        assert_eq!(
            arguments(&command),
            [
                "--model",
                "claude-opus-4-1",
                "--session-id",
                SESSION_A,
                "--add-dir",
                display(&fixture.handoff).as_str(),
                "--",
                PROMPT,
            ]
        );
        let cwd = command.get_current_dir().expect("cwd");
        assert_eq!(cwd, conventional_path(&fixture.workspace));
        assert!(!cwd.to_string_lossy().starts_with(r"\\?\"));
        assert!(cwd.to_string_lossy().contains("Work Files"));
        assert_eq!(
            config_dir(&command),
            Some(text(&fixture.home)),
            "an isolated home is passed explicitly"
        );
    }

    #[test]
    fn resumed_launch_delivers_the_prompt_into_the_session() {
        let fixture = LaunchFixture::new();
        let adapter = ClaudeAdapter::with_executable("claude");
        // The new-session ID is ignored when resuming.
        let command = adapter
            .handoff_command(
                &fixture.home,
                &fixture.workspace,
                &fixture.launch(Some(SESSION_B), Some(SESSION_A)),
            )
            .expect("command");
        assert_eq!(
            arguments(&command),
            [
                "--resume",
                SESSION_B,
                "--model",
                "claude-opus-4-1",
                "--add-dir",
                display(&fixture.handoff).as_str(),
                "--",
                PROMPT,
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(conventional_path(&fixture.workspace).as_path())
        );

        let mut minimal = fixture.launch(None, None);
        minimal.model = None;
        let command = adapter
            .handoff_command(&fixture.home, &fixture.workspace, &minimal)
            .expect("command");
        assert_eq!(
            arguments(&command),
            [
                "--add-dir",
                display(&fixture.handoff).as_str(),
                "--",
                PROMPT
            ]
        );
    }

    #[test]
    fn legacy_start_also_ends_options_before_its_prompt() {
        let fixture = LaunchFixture::new();
        let launch = legacy_launch(&fixture.handoff, None, Some("sonnet"));
        let command = ClaudeAdapter::with_executable("claude")
            .handoff_command(&fixture.home, &fixture.workspace, &launch)
            .expect("command");
        let arguments = arguments(&command);
        assert_eq!(
            arguments[..4],
            [
                "--model",
                "sonnet",
                "--add-dir",
                display(&fixture.handoff).as_str()
            ]
        );
        assert_eq!(arguments[4], "--");
        assert!(arguments[5].contains("context.md"));
        assert_eq!(arguments.len(), 6);
    }

    #[test]
    fn resume_runs_in_the_conventional_workspace() {
        let fixture = LaunchFixture::new();
        let command = ClaudeAdapter::with_executable("claude")
            .resume_command(&fixture.home, &fixture.workspace, SESSION_A)
            .expect("command");
        assert_eq!(arguments(&command), ["--resume", SESSION_A]);
        assert_eq!(
            command.get_current_dir(),
            Some(conventional_path(&fixture.workspace).as_path())
        );
    }

    #[test]
    fn invalid_launch_input_is_refused_before_a_command_exists() {
        let fixture = LaunchFixture::new();
        let adapter = ClaudeAdapter::with_executable("claude");
        let mut cases = Vec::new();
        for id in ["../escape", "-p", "", "a\\b"] {
            cases.push(fixture.launch(Some(id), None));
        }
        cases.push(fixture.launch(None, Some("not-a-uuid")));
        let mut model = fixture.launch(None, None);
        model.model = Some("--dangerously-skip-permissions".into());
        cases.push(model);
        let mut empty = fixture.launch(None, None);
        empty.prompt = " \n".into();
        cases.push(empty);
        let mut missing = fixture.launch(None, None);
        missing.handoff_directory = fixture.root.path().join("missing handoff");
        cases.push(missing);

        let before = commands_built_on_this_thread();
        for launch in &cases {
            let error = adapter
                .launch_with_handoff(&fixture.home, &fixture.workspace, launch)
                .expect_err("invalid launch");
            assert!(
                matches!(error, ContextWakeError::InvalidData(_)),
                "{launch:?}: {error}"
            );
        }
        assert!(
            adapter
                .resume(&fixture.home, &fixture.workspace, "../x")
                .is_err()
        );
        assert_eq!(commands_built_on_this_thread(), before);
    }

    #[test]
    fn missing_agent_is_reported_for_a_resumed_handoff() {
        let fixture = LaunchFixture::new();
        let adapter = ClaudeAdapter::discover_in(&DiscoveryContext::new(
            Some(fixture.root.path()),
            Some(fixture.root.path()),
            Vec::new(),
        ));
        let error = adapter
            .launch_with_handoff(
                &fixture.home,
                &fixture.workspace,
                &fixture.launch(Some(SESSION_A), None),
            )
            .expect_err("missing agent");
        // Not the trait's "cannot deliver a handoff into a resumed session".
        assert!(
            error.to_string().contains("CONTEXTWAKE_CLAUDE_BIN"),
            "{error}"
        );
    }

    #[test]
    fn launch_reports_the_agent_exit_status() {
        let fixture = LaunchFixture::new();
        let mut launch = fixture.launch(None, Some(SESSION_A));
        launch.prompt = "ContextWake handoff 1234".into();
        let adapter = |exit: &str| ClaudeAdapter {
            executable: AgentExecutable::Available(fake_agent(
                fixture.root.path(),
                &format!("exit /b {exit}"),
                &format!("exit {exit}"),
            )),
            homes: ConfigHomes::default(),
        };
        adapter("0")
            .launch_with_handoff(&fixture.home, &fixture.workspace, &launch)
            .expect("clean exit");
        let error = adapter("3")
            .launch_with_handoff(&fixture.home, &fixture.workspace, &launch)
            .expect_err("failed exit");
        assert!(error.to_string().contains("no native resume"), "{error}");
        launch.resume_session_id = Some(SESSION_B.into());
        let error = adapter("3")
            .launch_with_handoff(&fixture.home, &fixture.workspace, &launch)
            .expect_err("failed resume");
        assert!(error.to_string().contains("not confirmed"), "{error}");
    }

    // -- Transcripts -------------------------------------------------------

    #[test]
    fn project_directories_use_claude_codes_encoding() {
        assert_eq!(
            encode_project_directory(Path::new(r"C:\Users\Mike\AppData\Local\Temp\x")),
            "C--Users-Mike-AppData-Local-Temp-x"
        );
        assert_eq!(
            encode_project_directory(Path::new(r"D:\Mike Angelo Work Files\Café_app.v2")),
            "D--Mike-Angelo-Work-Files-Caf--app-v2"
        );
    }

    fn user(text: &str, timestamp: &str, cwd: &Path) -> Value {
        json!({
            "type": "user",
            "cwd": cwd.to_string_lossy(),
            "sessionId": "ignored",
            "timestamp": timestamp,
            "isSidechain": false,
            "message": {"role": "user", "content": text},
        })
    }

    fn assistant(id: &str, blocks: Value, timestamp: &str, cwd: &Path) -> Value {
        json!({
            "type": "assistant",
            "cwd": cwd.to_string_lossy(),
            "timestamp": timestamp,
            "message": {"id": id, "role": "assistant", "model": "claude-opus-4-1", "content": blocks},
        })
    }

    fn write_transcript(directory: &Path, session_id: &str, records: &[Value]) -> PathBuf {
        std::fs::create_dir_all(directory).expect("project directory");
        let path = directory.join(format!("{session_id}.jsonl"));
        let mut text = String::new();
        for record in records {
            text.push_str(&record.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).expect("transcript");
        path
    }

    /// A Claude home with a workspace whose path contains spaces.
    struct TranscriptFixture {
        _root: tempfile::TempDir,
        home: PathBuf,
        workspace: PathBuf,
        /// `cwd` as Claude Code records it.
        cwd: PathBuf,
        project: PathBuf,
    }

    impl TranscriptFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let home = root.path().join(".claude");
            let workspace = root.path().join("Work Files").join("my app");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let cwd = conventional_path(&workspace);
            let project = home
                .join(PROJECTS_DIRECTORY)
                .join(encode_project_directory(&cwd));
            Self {
                _root: root,
                home,
                workspace,
                cwd,
                project,
            }
        }

        fn adapter() -> ClaudeAdapter {
            ClaudeAdapter::with_executable("unused")
        }
    }

    #[test]
    fn sessions_are_listed_from_matching_transcripts_newest_first() {
        let fixture = TranscriptFixture::new();
        let cwd = &fixture.cwd;
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[
                json!({"type": "summary", "summary": "Old summary", "leafUuid": "x"}),
                user("Fix the build", "2026-11-01T10:00:00.000Z", cwd),
                assistant(
                    "m1",
                    json!([{"type": "text", "text": "Done"}]),
                    "2026-11-01T10:05:00Z",
                    cwd,
                ),
                json!({"type": "custom-title", "customTitle": "Build \u{1b}[31mfix", "sessionId": SESSION_A}),
            ],
        );
        write_transcript(
            &fixture.project,
            SESSION_B,
            &[
                json!({"type": "file-history-snapshot", "snapshot": {}}),
                user(&format!("{PROMPT}\nmore"), "2026-11-02T09:00:00Z", cwd),
                user("Add tests", "2026-11-02T09:01:00Z", cwd),
                json!("not an object"),
            ],
        );
        // Lossy encoding: another workspace in the same project directory.
        let other = fixture.cwd.parent().expect("parent").join("my-app");
        std::fs::create_dir_all(&other).expect("other workspace");
        write_transcript(
            &fixture.project,
            "11111111-2222-4333-8444-555555555555",
            &[user("Other project", "2026-11-03T09:00:00Z", &other)],
        );
        std::fs::write(fixture.project.join("notes.txt"), "x").expect("unrelated file");
        std::fs::write(fixture.project.join("bad name!.jsonl"), "{}").expect("invalid name");
        let mut garbage = vec![b'{'; 64];
        garbage.extend_from_slice(b"\n\xff\xfe\n");
        std::fs::write(
            fixture
                .project
                .join("22222222-2222-4333-8444-555555555555.jsonl"),
            garbage,
        )
        .expect("malformed transcript");

        let sessions = TranscriptFixture::adapter()
            .list_sessions(&fixture.home, &fixture.workspace, 10)
            .expect("sessions");
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.provider_session_id.as_str())
                .collect::<Vec<_>>(),
            [SESSION_B, SESSION_A]
        );
        let newest = &sessions[0];
        // ContextWake's own prompt is not used as the title.
        assert_eq!(newest.title.as_deref(), Some("Add tests"));
        assert_eq!(
            newest.workspace_path.as_deref(),
            Some(fixture.cwd.as_path())
        );
        assert_eq!(
            newest.created_at.map(|time| time.to_rfc3339()),
            Some("2026-11-02T09:00:00+00:00".into())
        );
        assert_eq!(
            newest.updated_at.map(|time| time.to_rfc3339()),
            Some("2026-11-02T09:01:00+00:00".into())
        );
        // The user's title wins over the summary and is terminal-safe.
        assert_eq!(sessions[1].title.as_deref(), Some("Build fix"));

        let limited = TranscriptFixture::adapter()
            .list_sessions(&fixture.home, &fixture.workspace, 1)
            .expect("sessions");
        assert_eq!(limited.len(), 1);
        assert!(
            TranscriptFixture::adapter()
                .list_sessions(&fixture.home, &fixture.workspace, 0)
                .expect("no sessions")
                .is_empty()
        );
    }

    #[test]
    fn listing_without_transcripts_is_empty() {
        let fixture = TranscriptFixture::new();
        let adapter = TranscriptFixture::adapter();
        assert!(
            adapter
                .list_sessions(&fixture.home, &fixture.workspace, 5)
                .expect("empty")
                .is_empty()
        );
        assert_eq!(
            adapter
                .conversation_excerpt(&fixture.home, &fixture.workspace, None)
                .expect("no excerpt"),
            None
        );
    }

    #[test]
    fn long_workspace_paths_also_match_shortened_directory_names() {
        let fixture = TranscriptFixture::new();
        let mut workspace = fixture.workspace.clone();
        while encode_project_directory(&conventional_path(&workspace)).len() <= MAX_ENCODED_NAME {
            workspace.push("nested directory");
        }
        // Stay below MAX_PATH so the fixture works without long-path support.
        std::fs::create_dir_all(&workspace).expect("long workspace");
        let encoded = encode_project_directory(&conventional_path(&workspace));
        let shortened = format!("{}-1a2b3c", &encoded[..MAX_ENCODED_NAME]);
        write_transcript(
            &fixture.home.join(PROJECTS_DIRECTORY).join(shortened),
            SESSION_A,
            &[user(
                "Deep work",
                "2026-11-01T10:00:00Z",
                &conventional_path(&workspace),
            )],
        );
        let sessions = TranscriptFixture::adapter()
            .list_sessions(&fixture.home, &workspace, 5)
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn excerpts_contain_typed_requests_and_the_latest_answer_only() {
        let fixture = TranscriptFixture::new();
        let cwd = &fixture.cwd;
        let at = "2026-11-01T10:00:00Z";
        let mut meta = user("Caveat: generated by local commands", at, cwd);
        meta["isMeta"] = json!(true);
        let mut sidechain = user("Subagent task", at, cwd);
        sidechain["isSidechain"] = json!(true);
        let mut compacted = user("This session is being continued", at, cwd);
        compacted["isCompactSummary"] = json!(true);
        let tool_result = json!({
            "type": "user", "cwd": cwd.to_string_lossy(), "timestamp": at,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "secret tool output"}
            ]},
            "toolUseResult": {"stdout": "secret tool output"},
        });
        let blocks = json!({
            "type": "user", "cwd": cwd.to_string_lossy(), "timestamp": at,
            "message": {"role": "user", "content": [
                {"type": "text", "text": "Use token=abcdef1234567890secret please"},
                {"type": "image", "source": {}},
                {"type": "text", "text": "<system-reminder>injected</system-reminder>"}
            ]},
        });
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[
                user("First request", at, cwd),
                user(PROMPT, at, cwd),
                meta,
                sidechain,
                compacted,
                user("<command-name>/model</command-name>", at, cwd),
                user(
                    "<local-command-stdout>Set model</local-command-stdout>",
                    at,
                    cwd,
                ),
                tool_result,
                blocks,
                assistant(
                    "m1",
                    json!([{"type": "thinking", "thinking": "hidden reasoning"}, {"type": "text", "text": "Older answer"}]),
                    at,
                    cwd,
                ),
                assistant(
                    "m2",
                    json!([{"type": "text", "text": "Final part one"}]),
                    at,
                    cwd,
                ),
                assistant(
                    "m2",
                    json!([{"type": "tool_use", "id": "t2", "name": "Bash", "input": {}}]),
                    at,
                    cwd,
                ),
                assistant(
                    "m2",
                    json!([{"type": "text", "text": "part two"}]),
                    "2026-11-01T10:09:00Z",
                    cwd,
                ),
                json!({"type": "assistant", "timestamp": at, "message": {"id": "m3", "model": "<synthetic>", "content": [{"type": "text", "text": "No response requested."}]}}),
            ],
        );
        let excerpt = TranscriptFixture::adapter()
            .conversation_excerpt(&fixture.home, &fixture.workspace, None)
            .expect("excerpt")
            .expect("transcript");
        assert_eq!(excerpt.agent_id, "claude");
        assert_eq!(excerpt.provider_session_id, SESSION_A);
        assert_eq!(excerpt.session_title.as_deref(), Some("First request"));
        assert_eq!(
            excerpt.user_requests.len(),
            3,
            "{:?}",
            excerpt.user_requests
        );
        assert_eq!(excerpt.user_requests[0], "First request");
        assert_eq!(excerpt.user_requests[1], DELIVERED_HANDOFF_REQUEST);
        assert!(
            excerpt.user_requests[2].contains("[REDACTED]")
                && !excerpt.user_requests[2].contains("abcdef1234567890secret"),
            "{}",
            excerpt.user_requests[2]
        );
        assert!(!excerpt.user_requests[2].contains("injected"));
        assert_eq!(
            excerpt.last_agent_message.as_deref(),
            Some("Final part one\npart two")
        );
        assert_eq!(
            excerpt.last_activity_at.map(|time| time.to_rfc3339()),
            Some("2026-11-01T10:09:00+00:00".into())
        );
        assert!(!excerpt.truncated);
        let text = serde_json::to_string(&excerpt).expect("json");
        for hidden in [
            "hidden reasoning",
            "secret tool output",
            "Subagent",
            "Caveat",
            "continued",
            "Set model",
        ] {
            assert!(!text.contains(hidden), "{hidden} leaked: {text}");
        }
    }

    #[test]
    fn excerpts_are_read_for_a_named_session_of_this_workspace_only() {
        let fixture = TranscriptFixture::new();
        let at = "2026-11-01T10:00:00Z";
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[user("Older session", at, &fixture.cwd)],
        );
        write_transcript(
            &fixture.project,
            SESSION_B,
            &[user("Newer session", "2026-11-02T10:00:00Z", &fixture.cwd)],
        );
        let elsewhere = fixture.home.parent().expect("root").join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        let foreign = "33333333-2222-4333-8444-555555555555";
        write_transcript(
            &fixture.project,
            foreign,
            &[user("Foreign", at, &elsewhere)],
        );

        let adapter = TranscriptFixture::adapter();
        let named = adapter
            .conversation_excerpt(&fixture.home, &fixture.workspace, Some(SESSION_A))
            .expect("excerpt")
            .expect("named session");
        assert_eq!(named.user_requests, ["Older session"]);
        assert_eq!(
            adapter
                .conversation_excerpt(&fixture.home, &fixture.workspace, Some(foreign))
                .expect("no excerpt"),
            None
        );
        assert_eq!(
            adapter
                .conversation_excerpt(
                    &fixture.home,
                    &fixture.workspace,
                    Some("44444444-2222-4333-8444-555555555555")
                )
                .expect("no excerpt"),
            None
        );
        assert!(
            adapter
                .conversation_excerpt(&fixture.home, &fixture.workspace, Some("../escape"))
                .is_err()
        );
    }

    #[test]
    fn session_ids_never_name_files_outside_the_project_directory() {
        let fixture = TranscriptFixture::new();
        let at = "2026-11-01T10:00:00Z";
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[user("Visible request", at, &fixture.cwd)],
        );
        for name in [
            "C:evil.jsonl",
            "c:evil.jsonl",
            &format!("{SESSION_A}.jsonl:s.jsonl"),
            &format!("{SESSION_A}.jsonl::$DATA.jsonl"),
        ] {
            assert!(
                TranscriptFile::at(&fixture.project, name).is_none(),
                "{name}"
            );
        }
        assert!(TranscriptFile::at(&fixture.project, &format!("{SESSION_A}.jsonl")).is_some());

        // On NTFS `<id>.jsonl:s` would open an alternate data stream of a
        // real transcript; it must not be read as a session.
        let stream = fixture.project.join(format!("{SESSION_A}.jsonl:s.jsonl"));
        let hidden = format!("{}\n", user("Hidden stream", at, &fixture.cwd));
        if cfg!(windows) {
            std::fs::write(&stream, hidden).expect("NTFS alternate data stream");
            let adapter = TranscriptFixture::adapter();
            let stream_id = format!("{SESSION_A}.jsonl:s");
            assert_eq!(
                adapter
                    .conversation_excerpt(&fixture.home, &fixture.workspace, Some(&stream_id))
                    .expect("no excerpt"),
                None
            );
            let record = LaunchRecord {
                started_at: DateTime::parse_from_rfc3339("2026-11-01T09:00:00Z")
                    .expect("time")
                    .with_timezone(&Utc),
                marker: "Hidden stream".into(),
                expected_session_id: Some(stream_id),
            };
            let launched = adapter
                .launched_session(&fixture.home, &fixture.workspace, &record)
                .expect("launched");
            assert!(launched.is_none_or(|session| !session.prompt_delivered));
        }
    }

    #[test]
    fn excerpts_of_long_transcripts_read_only_the_tail() {
        let fixture = TranscriptFixture::new();
        let filler = "x".repeat(4_096);
        let mut records = vec![user(
            "Very first request",
            "2026-11-01T10:00:00Z",
            &fixture.cwd,
        )];
        for index in 0..(EXCERPT_TAIL_BYTES / filler.len() + 8) {
            records.push(assistant(
                &format!("m{index}"),
                json!([{"type": "text", "text": filler}]),
                "2026-11-01T10:00:01Z",
                &fixture.cwd,
            ));
        }
        records.push(user("Latest request", "2026-11-01T11:00:00Z", &fixture.cwd));
        write_transcript(&fixture.project, SESSION_A, &records);
        let excerpt = TranscriptFixture::adapter()
            .conversation_excerpt(&fixture.home, &fixture.workspace, None)
            .expect("excerpt")
            .expect("transcript");
        assert_eq!(excerpt.user_requests, ["Latest request"]);
        assert!(excerpt.truncated);
        // The title still comes from the head.
        assert_eq!(excerpt.session_title.as_deref(), Some("Very first request"));
    }

    /// Launch time of the delivery tests, recent so that the fixture files
    /// written now count as modified after the launch.
    fn started_at() -> DateTime<Utc> {
        static START: std::sync::LazyLock<DateTime<Utc>> =
            std::sync::LazyLock::new(|| Utc::now() - chrono::Duration::minutes(10));
        *START
    }

    /// A transcript timestamp `seconds` after the launch.
    fn at(seconds: i64) -> String {
        (started_at() + chrono::Duration::seconds(seconds)).to_rfc3339()
    }

    fn record(expected: Option<&str>) -> LaunchRecord {
        LaunchRecord {
            started_at: started_at(),
            marker: MARKER.into(),
            expected_session_id: expected.map(str::to_string),
        }
    }

    const MARKER: &str =
        "ContextWake handoff 9d0c1b2a-0000-4000-8000-000000000001 \u{b7} launch 1a2b3c4d";

    #[test]
    fn delivery_is_confirmed_from_the_expected_session() {
        let fixture = TranscriptFixture::new();
        let prompt = format!("{MARKER}\nObjective: ship");
        // 1 s before the launch is within the tolerated clock skew.
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[user(&prompt, &at(-1), &fixture.cwd)],
        );
        let adapter = TranscriptFixture::adapter();
        let launched = adapter
            .launched_session(&fixture.home, &fixture.workspace, &record(Some(SESSION_A)))
            .expect("launched")
            .expect("session");
        assert_eq!(launched.provider_session_id, SESSION_A);
        assert!(launched.prompt_delivered);
        assert_eq!(launched.title, None, "the handoff prompt is not a title");

        // A marker-less record never confirms delivery.
        let mut blank = record(Some(SESSION_A));
        blank.marker = " ".into();
        let launched = adapter
            .launched_session(&fixture.home, &fixture.workspace, &blank)
            .expect("launched")
            .expect("session");
        assert!(!launched.prompt_delivered);
    }

    #[test]
    fn delivery_requires_a_typed_user_message_written_after_the_launch() {
        let fixture = TranscriptFixture::new();
        let prompt = format!("{MARKER}\nObjective: ship");
        let late = &at(5);
        let mut sidechain = user(&prompt, late, &fixture.cwd);
        sidechain["isSidechain"] = json!(true);
        let tool_result = json!({
            "type": "user", "cwd": fixture.cwd.to_string_lossy(), "timestamp": late,
            "message": {"role": "user", "content": [{"type": "tool_result", "content": prompt}]},
        });
        let mut untimed = user(&prompt, late, &fixture.cwd);
        untimed.as_object_mut().expect("object").remove("timestamp");
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[
                // An earlier launch's prompt (older than the skew allows).
                user(&prompt, &at(-3), &fixture.cwd),
                sidechain,
                tool_result,
                untimed,
                assistant(
                    "m1",
                    json!([{"type": "text", "text": MARKER}]),
                    late,
                    &fixture.cwd,
                ),
            ],
        );
        let launched = TranscriptFixture::adapter()
            .launched_session(&fixture.home, &fixture.workspace, &record(Some(SESSION_A)))
            .expect("launched")
            .expect("expected session is reported");
        assert_eq!(launched.provider_session_id, SESSION_A);
        assert!(!launched.prompt_delivered);
    }

    #[test]
    fn delivery_is_found_in_recent_sessions_when_no_id_was_assigned() {
        let fixture = TranscriptFixture::new();
        // Claude Code writes non-ASCII as-is, but an escaped marker is found too.
        let escaped = user(&format!("{MARKER}\nObjective"), &at(3), &fixture.cwd)
            .to_string()
            .replace('\u{b7}', "\\u00b7");
        assert!(escaped.contains("\\u00b7"));
        std::fs::create_dir_all(&fixture.project).expect("project");
        std::fs::write(
            fixture.project.join(format!("{SESSION_B}.jsonl")),
            format!("{escaped}\n"),
        )
        .expect("transcript");
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[user("Unrelated", &at(4), &fixture.cwd)],
        );
        let adapter = TranscriptFixture::adapter();
        let launched = adapter
            .launched_session(&fixture.home, &fixture.workspace, &record(None))
            .expect("launched")
            .expect("session");
        assert_eq!(launched.provider_session_id, SESSION_B);
        assert!(launched.prompt_delivered);

        // An expected session without the prompt does not hide the one that has it
        // (for example a resume that forked into a new session).
        let launched = adapter
            .launched_session(&fixture.home, &fixture.workspace, &record(Some(SESSION_A)))
            .expect("launched")
            .expect("session");
        assert_eq!(launched.provider_session_id, SESSION_B);
        assert!(launched.prompt_delivered);
    }

    #[test]
    fn nothing_is_guessed_without_a_match() {
        let fixture = TranscriptFixture::new();
        write_transcript(
            &fixture.project,
            SESSION_A,
            &[user("Unrelated", &at(4), &fixture.cwd)],
        );
        let adapter = TranscriptFixture::adapter();
        assert_eq!(
            adapter
                .launched_session(&fixture.home, &fixture.workspace, &record(None))
                .expect("launched"),
            None
        );
        assert_eq!(
            adapter
                .launched_session(&fixture.home, &fixture.workspace, &record(Some(SESSION_B)))
                .expect("launched"),
            None
        );
        assert!(
            adapter
                .launched_session(&fixture.home, &fixture.workspace, &record(Some("-bad")))
                .is_err()
        );
    }

    #[test]
    fn bounded_lines_skip_overlong_content() {
        let mut reader = std::io::Cursor::new(b"short\nmuch too long line\nlast".to_vec());
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 8).expect("read"),
            Some(true)
        );
        assert_eq!(line, b"short\n");
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 8).expect("read"),
            Some(false)
        );
        assert!(line.is_empty());
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 8).expect("read"),
            Some(true)
        );
        assert_eq!(line, b"last");
        assert_eq!(
            read_bounded_line(&mut reader, &mut line, 8).expect("read"),
            None
        );
    }

    #[test]
    fn marker_fragments_are_literal_in_every_json_encoding() {
        assert_eq!(
            literal_marker_fragment(MARKER),
            Some("ContextWake handoff 9d0c1b2a-0000-4000-8000-000000000001 ")
        );
        assert_eq!(literal_marker_fragment("\u{b7}short\u{b7}"), None);
    }
}
