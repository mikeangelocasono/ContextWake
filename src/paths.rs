use std::ffi::OsString;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{ContextWakeError, IoContext, Result};

/// Names inside the data directory. The state database records absolute
/// paths built from these names, so the store uses them to recognise its own
/// entries after the data directory moves.
pub(crate) const STATE_DB_FILE: &str = "state.sqlite3";
pub(crate) const AGENT_HOMES_DIR: &str = "agent-homes";
pub(crate) const CHECKPOINTS_DIR: &str = "checkpoints";
pub(crate) const HANDOFFS_DIR: &str = "handoffs";

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        if let Some(root) =
            state_root_override("CONTEXTWAKE_HOME", std::env::var_os("CONTEXTWAKE_HOME"))?
        {
            return Ok(Self::from_root(root));
        }
        // Pre-release compatibility: an explicitly supplied legacy root remains
        // authoritative. It is not moved because callers commonly use it for
        // disposable or portable state.
        if let Some(root) =
            state_root_override("AGENTDECK_HOME", std::env::var_os("AGENTDECK_HOME"))?
        {
            return Ok(Self::from_root(root));
        }

        let dirs = ProjectDirs::from("dev", "ContextWake", "ContextWake").ok_or_else(|| {
            ContextWakeError::Configuration(
                "the operating system did not provide application directories".into(),
            )
        })?;
        let paths = Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_local_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        };

        if let Some(legacy_dirs) = ProjectDirs::from("dev", "AgentDeck", "AgentDeck") {
            let legacy = Self {
                config_dir: legacy_dirs.config_dir().to_path_buf(),
                data_dir: legacy_dirs.data_local_dir().to_path_buf(),
                cache_dir: legacy_dirs.cache_dir().to_path_buf(),
            };
            paths.migrate_legacy_directories(&legacy)?;
        }

        Ok(paths)
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for path in [&self.config_dir, &self.data_dir, &self.cache_dir] {
            ensure_managed_directory(path)?;
        }
        for path in [
            self.agent_homes_dir(),
            self.checkpoints_dir(),
            self.handoffs_dir(),
            self.logs_dir(),
        ] {
            ensure_managed_directory(&path)?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_db(&self) -> PathBuf {
        self.data_dir.join(STATE_DB_FILE)
    }

    pub fn agent_homes_dir(&self) -> PathBuf {
        self.data_dir.join(AGENT_HOMES_DIR)
    }

    pub fn agent_home(&self, agent: &str, profile_id: &uuid::Uuid) -> PathBuf {
        self.agent_homes_dir()
            .join(agent)
            .join(profile_id.to_string())
    }

    /// Compatibility helper for databases and tests created before schema v3.
    pub fn provider_home(&self, provider: &str, profile_id: &uuid::Uuid) -> PathBuf {
        self.agent_home(provider, profile_id)
    }

    pub fn checkpoints_dir(&self) -> PathBuf {
        self.data_dir.join(CHECKPOINTS_DIR)
    }

    pub fn handoffs_dir(&self) -> PathBuf {
        self.data_dir.join(HANDOFFS_DIR)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn is_managed_path(&self, path: &Path) -> bool {
        path_is_within(path, &self.data_dir) || path_is_within(path, &self.config_dir)
    }

    /// Explains when the state directories may be readable by other local
    /// accounts, or returns `None` when they are private.
    ///
    /// Unix directories are restricted to the owner (0700) by [`Self::ensure`].
    /// `ContextWake` does not change Windows ACLs: the default per-user
    /// application directories are private to the account, but a custom
    /// `CONTEXTWAKE_HOME` outside the user profile (for example on a data
    /// volume) inherits the permissions of the folder it was created in, which
    /// often grant read access to every local user. Agent login state,
    /// checkpoints and handoffs live there.
    #[cfg_attr(not(windows), allow(clippy::unused_self))]
    pub fn permission_warning(&self) -> Option<String> {
        #[cfg(windows)]
        {
            let private_roots = std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .into_iter()
                .chain(
                    ProjectDirs::from("dev", "ContextWake", "ContextWake")
                        .map(|dirs| {
                            [
                                dirs.config_dir().to_path_buf(),
                                dirs.data_local_dir().to_path_buf(),
                                dirs.cache_dir().to_path_buf(),
                            ]
                        })
                        .into_iter()
                        .flatten(),
                )
                .collect::<Vec<_>>();
            let exposed = [&self.config_dir, &self.data_dir]
                .into_iter()
                .filter(|directory| {
                    !private_roots
                        .iter()
                        .any(|root| resolved_path_is_within(directory, root))
                })
                .map(|directory| directory.display().to_string())
                .collect::<Vec<_>>();
            if exposed.is_empty() {
                None
            } else {
                Some(format!(
                    "ContextWake state at {} is outside your user profile, so it keeps the permissions of the folder it was created in; other local accounts may be able to read agent logins, checkpoints and handoffs. Keep CONTEXTWAKE_HOME inside your user profile, or restrict the folder to your account.",
                    exposed.join(" and ")
                ))
            }
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn migrate_legacy_directories(&self, legacy: &Self) -> Result<()> {
        for (source, destination) in [
            (&legacy.config_dir, &self.config_dir),
            (&legacy.data_dir, &self.data_dir),
            (&legacy.cache_dir, &self.cache_dir),
        ] {
            if source == destination || destination.exists() || !source.exists() {
                continue;
            }
            let source_metadata = std::fs::symlink_metadata(source).at(source)?;
            if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
                return Err(ContextWakeError::UnsafePath(format!(
                    "legacy application directory must be a real directory: {}",
                    source.display()
                )));
            }
            let parent = destination.parent().ok_or_else(|| {
                ContextWakeError::UnsafePath(format!(
                    "new application directory has no parent: {}",
                    destination.display()
                ))
            })?;
            std::fs::create_dir_all(parent).at(parent)?;
            if std::fs::symlink_metadata(parent)
                .at(parent)?
                .file_type()
                .is_symlink()
            {
                return Err(ContextWakeError::UnsafePath(format!(
                    "new application directory parent must not be a symlink: {}",
                    parent.display()
                )));
            }
            std::fs::rename(source, destination).at(source)?;
        }
        Ok(())
    }
}

/// Interprets a state-root environment variable. An empty value counts as
/// unset. A relative value is refused: it would resolve against whichever
/// directory `ctx` happens to run in (often a repository), and coding agents,
/// which run inside the workspace, would resolve their configuration homes
/// against the workspace instead, writing login state into the project tree.
fn state_root_override(name: &str, value: Option<OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let root = PathBuf::from(value);
    if !root.is_absolute() {
        return Err(ContextWakeError::Configuration(format!(
            "{name} must be an absolute path, but it is set to '{}'. Set it to an absolute directory or unset it.",
            root.display()
        )));
    }
    // Normalizes separators and `.`/`..` on Windows; every stored path is
    // derived from this root, so it is kept in its conventional form.
    let root = std::path::absolute(&root).at(&root)?;
    Ok(Some(simplify_path(&root)))
}

/// Returns `path` in its conventional form, without the Windows verbatim
/// prefix (`\\?\C:\dir` becomes `C:\dir`, `\\?\UNC\server\share\dir` becomes
/// `\\server\share\dir`).
///
/// `std::fs::canonicalize` returns verbatim paths on Windows. Coding agents,
/// Git, shells and people expect the conventional form, and agents key their
/// local session stores by the working directory string, so a verbatim path
/// passed as an argument or compared with an agent-recorded `cwd` silently
/// breaks session discovery and native resume.
///
/// The input is returned unchanged on other platforms, for non-verbatim
/// paths, and whenever the conventional form could name a different file:
/// reserved device names, `.`/`..` components, trailing dots or spaces,
/// characters that are invalid in Win32 paths, or a length at or beyond the
/// classic `MAX_PATH` limit.
#[must_use]
pub fn simplify_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(simplified) = simplify_verbatim(path) {
            return simplified;
        }
    }
    path.to_path_buf()
}

/// Canonicalizes `path` (resolving symlinks and `..`) and returns the
/// conventional, non-verbatim form. See [`simplify_path`].
pub fn canonicalize_simplified(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize()
        .map(|canonical| simplify_path(&canonical))
}

/// Compares two paths the way the host filesystem does for already-resolved
/// paths: after removing Windows verbatim prefixes, Windows comparisons treat
/// `/` and `\` as equivalent and ignore ASCII case (the NTFS default); other
/// platforms compare exactly. Neither path is touched on disk.
#[must_use]
pub fn paths_equivalent(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            simplify_path(path)
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_ascii_lowercase()
        };
        normalize(left) == normalize(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// Like [`path_is_within`], but decided on the directories the paths resolve
/// to when both exist: 8.3 short names, `..` components, junctions and
/// symlinks are resolved first, so a junction under `root` that points to
/// another volume is outside it. Falls back to the lexical comparison when
/// either path cannot be canonicalized (for example, it does not exist yet).
#[cfg(windows)]
fn resolved_path_is_within(path: &Path, root: &Path) -> bool {
    match (canonicalize_simplified(path), canonicalize_simplified(root)) {
        (Ok(path), Ok(root)) => path_is_within(&path, &root),
        _ => path_is_within(path, root),
    }
}

/// True when `path` is `root` or lies inside it, using the same comparison
/// rules as [`paths_equivalent`]. Both paths should already be resolved.
#[must_use]
pub fn path_is_within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let mut value = simplify_path(path)
                .to_string_lossy()
                .replace('/', "\\")
                .to_ascii_lowercase();
            if !value.ends_with('\\') {
                value.push('\\');
            }
            value
        };
        normalize(path).starts_with(&normalize(root))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

#[cfg(windows)]
fn simplify_verbatim(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};

    // Conventional Win32 paths at or beyond MAX_PATH need the verbatim form
    // unless long-path support is enabled machine-wide, so keep them verbatim.
    const MAX_PATH: usize = 260;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let mut simplified = match prefix.kind() {
        Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => {
            format!("{}:", char::from(drive))
        }
        Prefix::VerbatimUNC(server, share) => {
            let server = server.to_str()?;
            let share = share.to_str()?;
            if !is_conventional_component(server) || !is_conventional_component(share) {
                return None;
            }
            format!(r"\\{server}\{share}")
        }
        _ => return None,
    };
    let mut has_root = false;
    for component in components {
        match component {
            Component::RootDir => has_root = true,
            Component::Normal(part) => {
                let part = part.to_str()?;
                if !is_conventional_component(part) {
                    return None;
                }
                simplified.push('\\');
                simplified.push_str(part);
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    if !has_root {
        return None;
    }
    if !simplified.contains('\\') || simplified.ends_with(':') {
        // A bare drive root such as `\\?\C:\` becomes `C:\`.
        simplified.push('\\');
    }
    (simplified.len() < MAX_PATH).then(|| PathBuf::from(simplified))
}

#[cfg(windows)]
fn is_conventional_component(part: &str) -> bool {
    const RESERVED: [&str; 24] = [
        "con", "prn", "aux", "nul", "conin$", "conout$", "com1", "com2", "com3", "com4", "com5",
        "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
        "lpt8", "lpt9",
    ];
    if part.is_empty() || part.ends_with('.') || part.ends_with(' ') {
        return false;
    }
    if part
        .chars()
        .any(|character| character < ' ' || r#"<>:"/\|?*"#.contains(character))
    {
        return false;
    }
    let stem = part
        .split('.')
        .next()
        .unwrap_or(part)
        .trim_end()
        .to_ascii_lowercase();
    let superscript_device = (stem.starts_with("com") || stem.starts_with("lpt"))
        && stem.len() > 3
        && stem[3..]
            .chars()
            .all(|character| matches!(character, '¹' | '²' | '³'));
    !RESERVED.contains(&stem.as_str()) && !superscript_device
}

fn ensure_managed_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).at(path)?;
    let metadata = std::fs::symlink_metadata(path).at(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ContextWakeError::UnsafePath(format!(
            "managed application directory must not be a symlink: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_directories_are_moved_without_overwriting_new_state() {
        let root = tempfile::tempdir().expect("root");
        let legacy = AppPaths::from_root(root.path().join("agentdeck"));
        let current = AppPaths::from_root(root.path().join("contextwake"));
        std::fs::create_dir_all(&legacy.config_dir).expect("legacy config");
        std::fs::create_dir_all(&legacy.data_dir).expect("legacy data");
        std::fs::write(legacy.config_file(), "legacy = true\n").expect("legacy file");
        std::fs::write(legacy.state_db(), "legacy-state").expect("legacy state");

        current
            .migrate_legacy_directories(&legacy)
            .expect("migration");
        assert_eq!(
            std::fs::read_to_string(current.config_file()).expect("new config"),
            "legacy = true\n"
        );
        assert_eq!(
            std::fs::read(current.state_db()).expect("new state"),
            b"legacy-state"
        );
        assert!(!legacy.config_dir.exists());
        assert!(!legacy.data_dir.exists());

        std::fs::create_dir_all(&legacy.config_dir).expect("second legacy config");
        std::fs::write(legacy.config_file(), "must-not-win\n").expect("second legacy file");
        current
            .migrate_legacy_directories(&legacy)
            .expect("idempotent migration");
        assert_eq!(
            std::fs::read_to_string(current.config_file()).expect("preserved config"),
            "legacy = true\n"
        );
    }

    #[test]
    fn legacy_state_database_keeps_working_after_the_directory_move() {
        use crate::checkpoint::CheckpointService;
        use crate::git::GitClient;
        use crate::model::{
            AuthState, Checkpoint, Profile, RedactionStatus, TrustState, Workspace,
        };
        use crate::store::Store;
        use chrono::Utc;
        use uuid::Uuid;

        let root = tempfile::tempdir().expect("root");
        let legacy = AppPaths::from_root(root.path().join("agentdeck"));
        let current = AppPaths::from_root(root.path().join("contextwake"));
        legacy.ensure().expect("legacy layout");
        let store = Store::open(legacy.state_db()).expect("legacy store");
        let now = Utc::now();
        let profile_id = Uuid::new_v4();
        store
            .insert_profile(&Profile {
                id: profile_id,
                name: "work".into(),
                display_name: "Work".into(),
                agent_id: "codex".into(),
                model_provider_id: None,
                model_preference: None,
                description: None,
                agent_home: legacy.agent_home("codex", &profile_id),
                home_mode: crate::model::ProfileHomeMode::Isolated,
                account_fingerprint: None,
                auth_state: AuthState::SignedIn,
                created_at: now,
                updated_at: now,
                last_used_at: None,
            })
            .expect("profile");
        std::fs::create_dir_all(legacy.agent_home("codex", &profile_id)).expect("agent home");
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: root.path().join("repository"),
            display_name: "repository".into(),
            trust_state: TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        };
        store.upsert_workspace(&workspace).expect("workspace");
        let checkpoint = Checkpoint {
            schema_version: 2,
            id: Uuid::new_v4(),
            workspace_id: workspace.id,
            session_id: None,
            agent_id: Some("codex".into()),
            model_provider_id: None,
            model: None,
            profile_id: Some(profile_id),
            objective: "Continue the migration".into(),
            active_task: None,
            completed: Vec::new(),
            decisions: Vec::new(),
            pending_tasks: Vec::new(),
            known_issues: Vec::new(),
            user_notes: None,
            project_instructions: None,
            files_changed: Vec::new(),
            git: None,
            validation_results: Vec::new(),
            created_at: now,
            redaction_status: RedactionStatus::Clean,
        };
        let checkpoint_file = legacy
            .checkpoints_dir()
            .join(format!("{}.json", checkpoint.id));
        std::fs::write(
            &checkpoint_file,
            serde_json::to_vec(&checkpoint).expect("checkpoint JSON"),
        )
        .expect("checkpoint file");
        store
            .insert_checkpoint_index(
                checkpoint.id,
                workspace.id,
                None,
                Some(profile_id),
                Some("codex"),
                None,
                None,
                &checkpoint.objective,
                &checkpoint_file,
                RedactionStatus::Clean,
                now,
            )
            .expect("checkpoint index");
        drop(store);

        current
            .migrate_legacy_directories(&legacy)
            .expect("migration");
        let store = Store::open(current.state_db()).expect("moved store");
        assert_eq!(
            store.profile("work").expect("profile").agent_home,
            current.agent_home("codex", &profile_id),
            "the agent keeps using the profile's moved login state"
        );
        let service = CheckpointService::new(
            store,
            current,
            GitClient::with_executable(
                "contextwake-test-missing-git",
                std::time::Duration::from_millis(10),
            ),
        );
        let listed = service.list().expect("checkpoints stay readable");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].objective, "Continue the migration");
    }

    #[test]
    fn state_root_overrides_must_be_absolute_and_empty_means_unset() {
        assert_eq!(state_root_override("CONTEXTWAKE_HOME", None).unwrap(), None);
        assert_eq!(
            state_root_override("CONTEXTWAKE_HOME", Some(OsString::new())).unwrap(),
            None
        );
        for relative in [".ctx", "state/home", "."] {
            let error =
                state_root_override("CONTEXTWAKE_HOME", Some(relative.into())).expect_err(relative);
            assert!(
                matches!(
                    &error,
                    ContextWakeError::Configuration(message)
                        if message.contains("CONTEXTWAKE_HOME") && message.contains("absolute")
                ),
                "{error}"
            );
        }
        let root = tempfile::tempdir().expect("root");
        assert_eq!(
            state_root_override("AGENTDECK_HOME", Some(root.path().as_os_str().to_owned()))
                .unwrap(),
            Some(root.path().to_path_buf())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_roots_are_absolute_and_conventional() {
        for (value, expected) in [
            (r"\\?\C:\ContextWake State", r"C:\ContextWake State"),
            ("C:/ContextWake/state", r"C:\ContextWake\state"),
            (r"C:\ContextWake\.\state\..\home", r"C:\ContextWake\home"),
        ] {
            assert_eq!(
                state_root_override("CONTEXTWAKE_HOME", Some(value.into())).unwrap(),
                Some(PathBuf::from(expected)),
                "{value}"
            );
        }
        // Drive-relative and root-relative paths depend on the current drive
        // or directory.
        for relative in [r"C:state", r"\state"] {
            assert!(
                state_root_override("CONTEXTWAKE_HOME", Some(relative.into())).is_err(),
                "{relative}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn custom_state_roots_outside_the_user_profile_are_reported() {
        let profile = PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"));
        assert_eq!(
            AppPaths::from_root(profile.join("contextwake-state")).permission_warning(),
            None
        );
        let dirs = ProjectDirs::from("dev", "ContextWake", "ContextWake").expect("dirs");
        let defaults = AppPaths {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_local_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        };
        assert_eq!(defaults.permission_warning(), None);
        let warning = AppPaths::from_root(r"Z:\contextwake-state")
            .permission_warning()
            .expect("warning for a data-volume root");
        assert!(warning.contains(r"Z:\contextwake-state"), "{warning}");
    }

    #[cfg(windows)]
    #[test]
    fn private_root_checks_compare_the_directories_paths_resolve_to() {
        let directory = tempfile::tempdir().expect("tempdir");
        let base = canonicalize_simplified(directory.path()).expect("canonical");
        let profile = base.join("profile");
        let outside = base.join("outside");
        let state = profile.join("state");
        std::fs::create_dir_all(profile.join("sub")).expect("profile");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::create_dir_all(&outside).expect("outside");

        // A different spelling of the profile root is still the profile.
        let respelled_root = profile.join("sub").join("..");
        assert!(!path_is_within(&state, &respelled_root));
        assert!(resolved_path_is_within(&state, &respelled_root));

        // Paths that do not exist yet are compared lexically.
        assert!(resolved_path_is_within(&profile.join("missing"), &profile));
        assert!(!resolved_path_is_within(&outside.join("missing"), &profile));

        // A junction under the profile that points elsewhere is not private.
        let junction = profile.join("linked state");
        let created = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !created {
            eprintln!("skipping the junction case: mklink /J is unavailable");
            return;
        }
        assert!(path_is_within(&junction, &profile));
        assert!(!resolved_path_is_within(&junction, &profile));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_state_directories_need_no_permission_warning() {
        assert_eq!(
            AppPaths::from_root("/srv/contextwake").permission_warning(),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn managed_path_checks_ignore_verbatim_prefix_and_case() {
        let paths = AppPaths::from_root(r"C:\Users\Example\ContextWake");
        assert!(paths.is_managed_path(Path::new(
            r"\\?\C:\users\example\contextwake\data\checkpoints\x.json"
        )));
        assert!(!paths.is_managed_path(Path::new(r"C:\Users\Example\ContextWakeOther\data")));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_are_simplified_only_when_lossless() {
        let cases = [
            (
                r"\\?\D:\Mike Angelo Work Files\ContextWake",
                r"D:\Mike Angelo Work Files\ContextWake",
            ),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\repo", r"\\server\share\repo"),
            (r"C:\already\conventional", r"C:\already\conventional"),
            (r"relative\path", r"relative\path"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                simplify_path(Path::new(input)),
                PathBuf::from(expected),
                "{input}"
            );
        }
        for lossy in [
            r"\\?\C:\dir\CON",
            r"\\?\C:\dir\nul.txt",
            r"\\?\C:\dir\COM¹",
            r"\\?\C:\trailing-dot.",
            r"\\?\C:\trailing-space ",
            r"\\?\C:\dir\..\other",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        ] {
            assert_eq!(
                simplify_path(Path::new(lossy)),
                PathBuf::from(lossy),
                "{lossy}"
            );
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(simplify_path(Path::new(&long)), PathBuf::from(&long));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_comparison_ignores_verbatim_prefix_case_and_separators() {
        assert!(paths_equivalent(
            Path::new(r"\\?\D:\Work\Repo"),
            Path::new(r"d:/work/repo/")
        ));
        assert!(!paths_equivalent(
            Path::new(r"D:\Work\Repo"),
            Path::new(r"D:\Work\Repo2")
        ));
        assert!(path_is_within(
            Path::new(r"\\?\D:\Work\Repo\bin\x.exe"),
            Path::new(r"d:\work\repo")
        ));
        assert!(path_is_within(
            Path::new(r"D:\Work\Repo"),
            Path::new(r"D:\Work\Repo")
        ));
        assert!(!path_is_within(
            Path::new(r"D:\Work\Repository"),
            Path::new(r"D:\Work\Repo")
        ));
    }

    #[test]
    fn canonicalized_paths_are_conventional() {
        let root = tempfile::tempdir().expect("root");
        let canonical = canonicalize_simplified(root.path()).expect("canonical");
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        assert!(paths_equivalent(
            &canonical,
            &simplify_path(&root.path().canonicalize().expect("raw"))
        ));
        assert!(path_is_within(&canonical.join("child"), &canonical));
    }

    #[cfg(unix)]
    #[test]
    fn managed_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path().join("contextwake"));
        paths.ensure().expect("paths");
        assert_eq!(
            std::fs::metadata(&paths.data_dir)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("root");
        let target = root.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let application_root = root.path().join("contextwake");
        std::fs::create_dir(&application_root).expect("application root");
        symlink(&target, application_root.join("config")).expect("symlink");
        let paths = AppPaths::from_root(application_root);
        assert!(matches!(
            paths.ensure(),
            Err(ContextWakeError::UnsafePath(_))
        ));
    }
}
