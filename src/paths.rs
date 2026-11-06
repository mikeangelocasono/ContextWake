use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{ContextWakeError, IoContext, Result};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        if let Some(root) = std::env::var_os("CONTEXTWAKE_HOME") {
            return Ok(Self::from_root(root));
        }
        // Pre-release compatibility: an explicitly supplied legacy root remains
        // authoritative. It is not moved because callers commonly use it for
        // disposable or portable state.
        if let Some(root) = std::env::var_os("AGENTDECK_HOME") {
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
        self.data_dir.join("state.sqlite3")
    }

    pub fn agent_homes_dir(&self) -> PathBuf {
        self.data_dir.join("agent-homes")
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
        self.data_dir.join("checkpoints")
    }

    pub fn handoffs_dir(&self) -> PathBuf {
        self.data_dir.join("handoffs")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn is_managed_path(&self, path: &Path) -> bool {
        path.starts_with(&self.data_dir) || path.starts_with(&self.config_dir)
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
