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
