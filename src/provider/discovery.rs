//! Coding-agent executable discovery.
//!
//! Discovery never runs anything. It resolves an absolute native executable
//! (or, for npm packages that ship a JavaScript entry point, a fixed
//! interpreter plus script) from the user's `PATH`, or records explicitly that
//! the agent is missing. A missing agent has no path at all, so no code path
//! can spawn a placeholder that another local user could create.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{ContextWakeError, Result};
use crate::model::{AgentHealth, AuthState};
use crate::provider::AcpTransport;
use crate::security::sanitize_terminal;

#[cfg(test)]
thread_local! {
    static COMMANDS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Number of agent commands built on the calling thread. Every agent process
/// is created from [`Launcher::command`], so a test can prove that a code path
/// spawned nothing.
#[cfg(test)]
pub(crate) fn commands_built_on_this_thread() -> usize {
    COMMANDS_BUILT.with(std::cell::Cell::get)
}

/// How an adapter starts its coding-agent CLI: an absolute program, fixed
/// leading arguments (an npm package's script for `node.exe`) and fixed
/// environment adjustments that the agent's official launcher would apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launcher {
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, Option<OsString>)>,
    reported: PathBuf,
}

impl Launcher {
    pub(crate) fn direct(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        Self {
            reported: program.clone(),
            program,
            arguments: Vec::new(),
            environment: Vec::new(),
        }
    }

    /// `interpreter <script>`, the way npm's command shims start JavaScript
    /// CLIs. The script is what users see as the executable.
    pub(crate) fn script(interpreter: impl Into<PathBuf>, script: impl Into<PathBuf>) -> Self {
        let script = script.into();
        Self {
            program: interpreter.into(),
            arguments: vec![script.clone().into_os_string()],
            environment: Vec::new(),
            reported: script,
        }
    }

    /// A program with arbitrary fixed leading arguments; tests use it to run
    /// deterministic fake agents through `cmd.exe` or `sh`.
    #[cfg(test)]
    pub(crate) fn with_arguments<I, S>(program: impl Into<PathBuf>, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut launcher = Self::direct(program);
        launcher.arguments = arguments.into_iter().map(Into::into).collect();
        launcher
    }

    #[must_use]
    pub(crate) fn with_environment(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment.push((key.into(), Some(value.into())));
        self
    }

    #[must_use]
    pub(crate) fn without_environment(mut self, key: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), None));
        self
    }

    #[cfg(all(test, windows))]
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn reported_path(&self) -> &Path {
        &self.reported
    }

    #[cfg(all(test, windows))]
    pub(crate) fn environment_value(&self, key: &str) -> Option<&OsString> {
        self.environment
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_ref())
    }

    /// The only place agent processes are built. No shell is involved.
    pub(crate) fn command(&self) -> Command {
        #[cfg(test)]
        COMMANDS_BUILT.with(|count| count.set(count.get() + 1));
        let mut command = Command::new(&self.program);
        command.args(&self.arguments);
        for (key, value) in &self.environment {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        command
    }

    pub(crate) fn acp_transport(&self) -> AcpTransport {
        let mut transport = AcpTransport::new(&self.program);
        for argument in &self.arguments {
            transport = transport.argument(argument.clone());
        }
        for (key, value) in &self.environment {
            if let Some(value) = value {
                transport = transport.environment(key.clone(), value.clone());
            }
        }
        transport
    }
}

/// The outcome of executable discovery for one adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentExecutable {
    Available(Launcher),
    /// Nothing can be started. `reason` tells the user how to fix it;
    /// `configured` is the rejected override, when there was one.
    Missing {
        reason: String,
        configured: Option<PathBuf>,
    },
}

impl AgentExecutable {
    pub(crate) fn direct(program: impl Into<PathBuf>) -> Self {
        Self::Available(Launcher::direct(program))
    }

    pub(crate) fn launcher(&self) -> Option<&Launcher> {
        match self {
            Self::Available(launcher) => Some(launcher),
            Self::Missing { .. } => None,
        }
    }

    /// Builds a command for an available agent. A missing agent yields a
    /// capability error before any `Command` exists.
    pub(crate) fn command(&self) -> Result<Command> {
        match self {
            Self::Available(launcher) => Ok(launcher.command()),
            Self::Missing { reason, .. } => {
                Err(ContextWakeError::CapabilityUnavailable(reason.clone()))
            }
        }
    }

    pub(crate) fn reported_path(&self) -> Option<PathBuf> {
        self.launcher()
            .map(|launcher| launcher.reported_path().to_path_buf())
    }

    /// Health for an agent that cannot be started, or `None` when a launcher
    /// exists and detection has to probe it.
    pub(crate) fn unavailable_health(&self, agent_id: &str) -> Option<AgentHealth> {
        match self {
            Self::Available(_) => None,
            Self::Missing { reason, configured } => Some(AgentHealth {
                agent_id: agent_id.into(),
                installed: false,
                executable: configured.clone(),
                version: None,
                auth_state: AuthState::Unknown,
                message: reason.clone(),
            }),
        }
    }
}

/// Static discovery facts for one adapter.
pub(crate) struct AgentBinary {
    pub(crate) name: &'static str,
    pub(crate) install_hint: &'static str,
    /// Override variables in precedence order; the first is the documented one.
    pub(crate) variables: &'static [&'static str],
}

impl AgentBinary {
    /// Uses the configured override when one is set, otherwise `discover`.
    /// `resolve_wrapper` maps a `.cmd`/`.bat` override to the native program
    /// or interpreter it would start, so the wrapper itself never runs.
    pub(crate) fn resolve(
        &self,
        context: &DiscoveryContext,
        resolve_wrapper: impl FnOnce(&Path, &DiscoveryContext) -> Option<Launcher>,
        discover: impl FnOnce(&DiscoveryContext) -> Option<Launcher>,
    ) -> AgentExecutable {
        if let Some((variable, value)) = context.configured(self.variables) {
            return self.configured(variable, PathBuf::from(value), |wrapper| {
                resolve_wrapper(wrapper, context)
            });
        }
        discover(context).map_or_else(|| self.not_found(), AgentExecutable::Available)
    }

    fn configured(
        &self,
        variable: &str,
        path: PathBuf,
        resolve_wrapper: impl FnOnce(&Path) -> Option<Launcher>,
    ) -> AgentExecutable {
        if !is_batch_wrapper(&path) {
            return AgentExecutable::direct(path);
        }
        match resolve_wrapper(&path) {
            Some(launcher) => AgentExecutable::Available(launcher),
            None => AgentExecutable::Missing {
                reason: format!(
                    "{variable} points at a Windows batch wrapper ({}). ContextWake never runs .cmd or .bat wrappers because cmd.exe reinterprets prompt and session arguments; set {variable} to the native {} executable.",
                    sanitize_terminal(&path.display().to_string()),
                    self.name
                ),
                configured: Some(path),
            },
        }
    }

    fn not_found(&self) -> AgentExecutable {
        AgentExecutable::Missing {
            reason: format!(
                "{} could not be found on PATH. {} or set {} to its executable path.",
                self.name, self.install_hint, self.variables[0]
            ),
            configured: None,
        }
    }
}

fn is_batch_wrapper(path: &Path) -> bool {
    cfg!(windows)
        && path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[derive(Clone, Debug)]
enum Overrides {
    /// Read `CONTEXTWAKE_*_BIN` from the process environment.
    Process,
    /// Fixed values; tests never mutate the process environment.
    Fixed(BTreeMap<String, OsString>),
}

/// Everything discovery may look at. [`DiscoveryContext::new`] takes the
/// working directory, home directory and `PATH` entries as inputs and reads no
/// process state, so the repository guard is testable without touching the
/// real environment.
#[derive(Clone, Debug)]
pub(crate) struct DiscoveryContext {
    search_path: Vec<PathBuf>,
    protected_roots: Vec<PathBuf>,
    overrides: Overrides,
    /// The user's home directory, the base of agents' default homes.
    home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, where Cursor's Windows installer puts its bundle.
    #[cfg(windows)]
    local_app_data: Option<PathBuf>,
}

impl DiscoveryContext {
    pub(crate) fn from_environment() -> Self {
        let working_directory = std::env::current_dir().ok();
        let path_entries = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut context = Self::new(
            working_directory.as_deref(),
            home_directory().as_deref(),
            path_entries,
        );
        context.overrides = Overrides::Process;
        #[cfg(windows)]
        {
            context.local_app_data = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute());
        }
        context
    }

    /// Builds a context from explicit inputs. Relative `PATH` entries (such as
    /// `.` or `node_modules\.bin`) resolve against whatever directory the user
    /// happens to be in, so they are dropped here and never searched.
    pub(crate) fn new(
        working_directory: Option<&Path>,
        home: Option<&Path>,
        path_entries: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            search_path: path_entries
                .into_iter()
                .filter(|entry| entry.is_absolute())
                .collect(),
            protected_roots: protected_roots(working_directory, home),
            overrides: Overrides::Fixed(BTreeMap::new()),
            home: home
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf),
            #[cfg(windows)]
            local_app_data: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_override(mut self, variable: &str, value: impl Into<OsString>) -> Self {
        if let Overrides::Fixed(values) = &mut self.overrides {
            values.insert(variable.into(), value.into());
        }
        self
    }

    #[cfg(all(test, windows))]
    #[must_use]
    pub(crate) fn with_local_app_data(mut self, path: impl Into<PathBuf>) -> Self {
        self.local_app_data = Some(path.into());
        self
    }

    /// Absolute `PATH` directories in search order. Windows adapters walk them
    /// to resolve npm shims to what the shim would start.
    #[cfg(any(windows, test))]
    pub(crate) fn search_path(&self) -> &[PathBuf] {
        &self.search_path
    }

    #[cfg(test)]
    pub(crate) fn protected_roots(&self) -> &[PathBuf] {
        &self.protected_roots
    }

    #[cfg(windows)]
    pub(crate) fn local_app_data(&self) -> Option<&Path> {
        self.local_app_data.as_deref()
    }

    /// The configuration home an agent uses when `ContextWake` sets no
    /// override: the absolute directory named by `variable` (for example
    /// `CLAUDE_CONFIG_DIR`) when it is set, else `directory` inside the
    /// user's home (the home itself when `directory` is empty). Like the
    /// executable overrides, `variable` comes from the process environment
    /// only in contexts built by [`Self::from_environment`].
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "wave 2: used by the Claude, Codex and Gemini default homes"
        )
    )]
    pub(crate) fn agent_default_home(&self, variable: &str, directory: &str) -> Option<PathBuf> {
        let configured = match &self.overrides {
            Overrides::Process => std::env::var_os(variable),
            Overrides::Fixed(values) => values.get(variable).cloned(),
        }
        .map(strip_wrapping_quotes)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
        configured.or_else(|| {
            self.home.as_ref().map(|home| {
                if directory.is_empty() {
                    home.clone()
                } else {
                    home.join(directory)
                }
            })
        })
    }

    /// First non-empty override among `variables`. On Windows, quotes copied
    /// around a path (`set X="C:\...\codex.exe"`) are removed.
    fn configured(&self, variables: &[&'static str]) -> Option<(&'static str, OsString)> {
        variables.iter().find_map(|variable| {
            let value = match &self.overrides {
                Overrides::Process => std::env::var_os(variable),
                Overrides::Fixed(values) => values.get(*variable).cloned(),
            }?;
            let value = strip_wrapping_quotes(value);
            (!value.is_empty()).then_some((*variable, value))
        })
    }

    /// First `PATH` directory that contains a safe `file_name`. The returned
    /// path is the `PATH` entry joined with the name, not its resolved target,
    /// so multi-call binaries that dispatch on `argv[0]` keep working.
    pub(crate) fn find_on_path(&self, file_name: &str) -> Option<PathBuf> {
        self.search_path
            .iter()
            .map(|directory| directory.join(file_name))
            .find(|candidate| self.is_safe_executable(candidate))
    }

    /// An existing executable outside every protected repository, judged by
    /// both the path as found and its resolved target (so a symlink or
    /// junction cannot smuggle a repository binary in).
    pub(crate) fn is_safe_executable(&self, candidate: &Path) -> bool {
        is_executable_file(candidate) && self.is_outside_repositories(candidate)
    }

    /// Like [`Self::is_safe_executable`] for files that are read or passed to
    /// an interpreter rather than executed (scripts, npm shims).
    #[cfg(windows)]
    pub(crate) fn is_safe_file(&self, candidate: &Path) -> bool {
        candidate.is_file() && self.is_outside_repositories(candidate)
    }

    fn is_outside_repositories(&self, candidate: &Path) -> bool {
        candidate.is_absolute()
            && !self.is_protected(candidate)
            && candidate
                .canonicalize()
                .is_ok_and(|resolved| !self.is_protected(&resolved))
    }

    fn is_protected(&self, path: &Path) -> bool {
        self.protected_roots
            .iter()
            .any(|root| is_within(path, root))
    }
}

fn home_directory() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|directories| directories.home_dir().to_path_buf())
        .filter(|path| path.is_absolute())
}

#[cfg(windows)]
fn strip_wrapping_quotes(value: OsString) -> OsString {
    match value.to_str() {
        Some(text) if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') => {
            OsString::from(&text[1..text.len() - 1])
        }
        _ => value,
    }
}

#[cfg(not(windows))]
fn strip_wrapping_quotes(value: OsString) -> OsString {
    value
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Every Git work tree that encloses `working_directory`, in both the given
/// and the resolved form. Executables inside them are repository-controlled
/// and are never picked up implicitly.
///
/// The user's home directory, its ancestors and filesystem roots are never
/// protected, even when one of them is a Git work tree (a dotfiles
/// repository): per-user installations such as `~/.local/bin`, `%APPDATA%\npm`
/// or `%LOCALAPPDATA%\Programs` live there. A working directory outside any
/// Git work tree protects nothing; relative `PATH` entries are dropped
/// separately by [`DiscoveryContext::new`].
///
/// The function only inspects the given directories for a `.git` entry; it
/// reads no environment variables and runs no Git command.
pub(crate) fn protected_roots(
    working_directory: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let Some(working_directory) = working_directory.filter(|path| path.is_absolute()) else {
        return Vec::new();
    };
    let homes = home
        .filter(|path| path.is_absolute())
        .map(path_forms)
        .unwrap_or_default();
    let unprotectable = |directory: &Path| {
        directory.parent().is_none() || homes.iter().any(|home| is_within(home, directory))
    };
    let mut roots = Vec::new();
    for start in path_forms(working_directory) {
        for directory in start.ancestors() {
            if unprotectable(directory) {
                // Every further ancestor is an ancestor of the home directory
                // or there is none.
                break;
            }
            if std::fs::symlink_metadata(directory.join(".git")).is_ok() {
                for root in path_forms(directory) {
                    push_unique(&mut roots, root);
                }
            }
        }
    }
    roots
}

/// `path` and, when it resolves, its canonical form.
fn path_forms(path: &Path) -> Vec<PathBuf> {
    let mut forms = vec![path.to_path_buf()];
    if let Ok(canonical) = path.canonicalize() {
        push_unique(&mut forms, canonical);
    }
    forms
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths
        .iter()
        .any(|existing| is_within(existing, &path) && is_within(&path, existing))
    {
        paths.push(path);
    }
}

/// True when `path` is `root` or lies inside it. On Windows the comparison
/// ignores case, separator style and verbatim (`\\?\`) prefixes. Unlike
/// [`crate::paths::path_is_within`] it also strips verbatim prefixes from
/// paths at or beyond `MAX_PATH`: this is only a comparison, so the lossless
/// rule that keeps such paths verbatim for use does not apply, and a long
/// resolved path inside a repository must still count as inside it.
#[cfg(windows)]
fn is_within(path: &Path, root: &Path) -> bool {
    fn comparable(path: &Path) -> String {
        let text = path.to_string_lossy().replace('/', "\\");
        let mut text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            text
        }
        .to_lowercase();
        if !text.ends_with('\\') {
            text.push('\\');
        }
        text
    }
    comparable(path).starts_with(&comparable(root))
}

#[cfg(not(windows))]
fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn agent_default_homes_prefer_an_absolute_variable_then_the_user_home() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let configured = root.path().join("claude-config");
        let context = DiscoveryContext::new(Some(root.path()), Some(&home), Vec::new());
        assert_eq!(
            context.agent_default_home("CLAUDE_CONFIG_DIR", ".claude"),
            Some(home.join(".claude"))
        );
        assert_eq!(
            context.agent_default_home("GEMINI_CLI_HOME", ""),
            Some(home.clone())
        );
        let configured_context = DiscoveryContext::new(Some(root.path()), Some(&home), Vec::new())
            .with_override("CLAUDE_CONFIG_DIR", configured.as_os_str())
            .with_override("CODEX_HOME", "relative-codex-home");
        assert_eq!(
            configured_context.agent_default_home("CLAUDE_CONFIG_DIR", ".claude"),
            Some(configured)
        );
        // A relative value is not a usable home; the default applies.
        assert_eq!(
            configured_context.agent_default_home("CODEX_HOME", ".codex"),
            Some(home.join(".codex"))
        );
        let homeless = DiscoveryContext::new(Some(root.path()), None, Vec::new());
        assert_eq!(homeless.agent_default_home("CODEX_HOME", ".codex"), None);
    }

    /// Creates `path` (and its parents) as a file that discovery accepts as an
    /// executable on every platform.
    pub(crate) fn executable(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture parent");
        std::fs::write(path, b"fixture").expect("fixture executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("fixture permissions");
        }
        path.to_path_buf()
    }

    pub(crate) fn git_work_tree(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.join(".git")).expect("fixture work tree");
        path.to_path_buf()
    }

    fn name(base: &str) -> String {
        if cfg!(windows) {
            format!("{base}.exe")
        } else {
            base.into()
        }
    }

    fn contains(roots: &[PathBuf], expected: &Path) -> bool {
        roots
            .iter()
            .any(|root| is_within(root, expected) && is_within(expected, root))
    }

    #[test]
    fn protected_roots_cover_enclosing_work_trees_but_never_home_or_filesystem_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = git_work_tree(&temp.path().join("home"));
        let superproject = git_work_tree(&home.join("projects").join("super"));
        let submodule = git_work_tree(&superproject.join("vendor").join("module"));
        let working_directory = submodule.join("src");
        std::fs::create_dir_all(&working_directory).expect("working directory");

        let roots = protected_roots(Some(&working_directory), Some(&home));
        assert!(contains(&roots, &submodule));
        assert!(contains(&roots, &superproject));
        assert!(
            !roots.iter().any(|root| is_within(&home, root)),
            "the home directory is a dotfiles work tree but must never be protected: {roots:?}"
        );

        assert!(protected_roots(Some(&home), Some(&home)).is_empty());
        let plain = home.join("Documents");
        std::fs::create_dir_all(&plain).expect("plain directory");
        assert!(protected_roots(Some(&plain), Some(&home)).is_empty());
        assert!(protected_roots(None, Some(&home)).is_empty());
        assert!(protected_roots(Some(Path::new("relative")), Some(&home)).is_empty());
        let filesystem_root = working_directory
            .ancestors()
            .last()
            .expect("filesystem root")
            .to_path_buf();
        assert!(protected_roots(Some(&filesystem_root), Some(&home)).is_empty());
    }

    #[test]
    fn user_installs_are_found_when_started_from_the_home_directory() {
        // Old rule: everything under the canonical working directory was
        // rejected, so starting `ctx` in the home directory hid every per-user
        // installation.
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let local_bin = home.join(".local").join("bin");
        let claude = executable(&local_bin.join(name("claude")));
        let context = DiscoveryContext::new(Some(&home), Some(&home), [local_bin]);
        assert!(context.protected_roots().is_empty());
        assert_eq!(context.find_on_path(&name("claude")), Some(claude));
    }

    #[test]
    fn installs_below_a_plain_working_directory_are_found() {
        // Old rule: a working directory that is an ancestor of the install
        // directory (here `%LOCALAPPDATA%`) rejected the install.
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let local = home.join("AppData").join("Local");
        let codex_bin = local
            .join("Programs")
            .join("OpenAI")
            .join("Codex")
            .join("bin");
        let codex = executable(&codex_bin.join(name("codex")));
        let context = DiscoveryContext::new(Some(&local), Some(&home), [codex_bin]);
        assert_eq!(context.find_on_path(&name("codex")), Some(codex));
    }

    #[test]
    fn repository_bin_on_path_is_rejected_from_a_subdirectory() {
        // Old rule: from `repo/sub` only `repo/sub` was excluded, so an
        // absolute `repo/bin` PATH entry (direnv, mise, manual edits) won.
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repository = git_work_tree(&home.join("repo"));
        let working_directory = repository.join("sub");
        std::fs::create_dir_all(&working_directory).expect("subdirectory");
        let hostile_bin = repository.join("bin");
        executable(&hostile_bin.join(name("codex")));
        let tools = home.join("tools");
        let trusted = executable(&tools.join(name("codex")));

        let context = DiscoveryContext::new(
            Some(&working_directory),
            Some(&home),
            [hostile_bin.clone(), tools],
        );
        assert_eq!(context.find_on_path(&name("codex")), Some(trusted));
        assert!(!context.is_safe_executable(&hostile_bin.join(name("codex"))));
    }

    #[test]
    fn relative_path_entries_are_never_searched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let working_directory = home.join("checkout");
        executable(&working_directory.join(name("codex")));
        executable(&working_directory.join("bin").join(name("codex")));
        let mut entries = vec![PathBuf::from("."), PathBuf::from("bin")];
        if cfg!(windows) {
            entries.push(PathBuf::from(r"C:bin"));
        }
        let context = DiscoveryContext::new(Some(&working_directory), Some(&home), entries);
        assert!(context.search_path().is_empty());
        assert_eq!(context.find_on_path(&name("codex")), None);
    }

    #[cfg(windows)]
    #[test]
    fn long_resolved_paths_still_count_as_inside_a_repository() {
        // `simplify_path` keeps paths at or beyond MAX_PATH verbatim, so a
        // conventional root and a long verbatim candidate must still compare.
        let long = format!(r"\\?\C:\Work\Repo\{}\codex.exe", "a".repeat(300));
        assert!(is_within(Path::new(&long), Path::new(r"c:\work\repo")));
        assert!(is_within(
            Path::new(r"\\?\UNC\server\share\repo\bin"),
            Path::new(r"\\server\share\repo")
        ));
        assert!(!is_within(
            Path::new(r"C:\Work\Repository\codex.exe"),
            Path::new(r"C:\Work\Repo")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn junction_from_outside_into_a_repository_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repository = git_work_tree(&home.join("repo"));
        let inside = repository.join("bin");
        executable(&inside.join("codex.exe"));
        let junction = home.join("looks-safe");
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let status = Command::new(PathBuf::from(system_root).join(r"System32\cmd.exe"))
            .arg("/d")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(&junction)
            .arg(&inside)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J failed");

        let context = DiscoveryContext::new(Some(&repository), Some(&home), [junction.clone()]);
        assert!(junction.join("codex.exe").is_file());
        assert_eq!(context.find_on_path("codex.exe"), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_from_outside_into_a_repository_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repository = git_work_tree(&home.join("repo"));
        let inside = repository.join("bin");
        executable(&inside.join("codex"));
        let link = home.join("looks-safe");
        std::os::unix::fs::symlink(&inside, &link).expect("symlink");

        let context = DiscoveryContext::new(Some(&repository), Some(&home), [link.clone()]);
        assert!(link.join("codex").is_file());
        assert_eq!(context.find_on_path("codex"), None);
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_non_executable_files_and_keeps_the_path_name() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let first = home.join("first");
        std::fs::create_dir_all(&first).expect("first");
        std::fs::write(first.join("codex"), b"not executable").expect("plain file");
        std::fs::set_permissions(first.join("codex"), std::fs::Permissions::from_mode(0o644))
            .expect("permissions");
        let target = executable(&home.join("multicall").join("mise"));
        let shims = home.join("shims");
        std::fs::create_dir_all(&shims).expect("shims");
        std::os::unix::fs::symlink(&target, shims.join("codex")).expect("shim symlink");

        let context = DiscoveryContext::new(Some(&home), Some(&home), [first, shims.clone()]);
        // The symlink itself is returned so argv[0] stays `codex`.
        assert_eq!(context.find_on_path("codex"), Some(shims.join("codex")));
    }

    #[test]
    fn overrides_win_and_batch_wrappers_are_never_used_directly() {
        const CODEX: AgentBinary = AgentBinary {
            name: "Codex CLI",
            install_hint: "Install Codex CLI",
            variables: &["CONTEXTWAKE_CODEX_BIN", "AGENTDECK_CODEX_BIN"],
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let found = executable(&temp.path().join("bin").join(name("codex")));
        let discover =
            |context: &DiscoveryContext| context.find_on_path(&name("codex")).map(Launcher::direct);
        let base = DiscoveryContext::new(
            Some(temp.path()),
            Some(temp.path()),
            [temp.path().join("bin")],
        );

        assert_eq!(
            CODEX.resolve(&base, |_, _| None, discover),
            AgentExecutable::direct(&found)
        );
        let legacy = base
            .clone()
            .with_override("CONTEXTWAKE_CODEX_BIN", "")
            .with_override("AGENTDECK_CODEX_BIN", "legacy-codex");
        assert_eq!(
            CODEX.resolve(&legacy, |_, _| None, discover),
            AgentExecutable::direct("legacy-codex")
        );
        let missing = DiscoveryContext::new(Some(temp.path()), Some(temp.path()), Vec::new());
        let AgentExecutable::Missing { reason, configured } =
            CODEX.resolve(&missing, |_, _| None, discover)
        else {
            panic!("an empty PATH must not produce a launcher");
        };
        assert!(reason.contains("CONTEXTWAKE_CODEX_BIN"), "{reason}");
        assert!(configured.is_none());

        if cfg!(windows) {
            let wrapper = base
                .clone()
                .with_override("CONTEXTWAKE_CODEX_BIN", r#""C:\npm\codex.cmd""#);
            let AgentExecutable::Missing { reason, configured } =
                CODEX.resolve(&wrapper, |_, _| None, discover)
            else {
                panic!("an unresolvable batch wrapper must not be started");
            };
            assert!(reason.contains("batch wrapper"), "{reason}");
            assert_eq!(configured, Some(PathBuf::from(r"C:\npm\codex.cmd")));
            assert_eq!(
                CODEX.resolve(&wrapper, |_, _| Some(Launcher::direct(&found)), discover),
                AgentExecutable::direct(&found)
            );
        }
    }

    #[test]
    fn missing_executables_build_no_command() {
        let missing = AgentExecutable::Missing {
            reason: "Codex CLI could not be found on PATH.".into(),
            configured: None,
        };
        let before = commands_built_on_this_thread();
        assert!(matches!(
            missing.command(),
            Err(ContextWakeError::CapabilityUnavailable(_))
        ));
        let health = missing.unavailable_health("codex").expect("health");
        assert!(!health.installed);
        assert!(health.executable.is_none());
        assert_eq!(commands_built_on_this_thread(), before);
    }
}
