use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ContextWakeError, IoContext, Result};
use crate::paths::AppPaths;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub schema_version: u32,
    pub telemetry: bool,
    pub retention_days: u32,
    pub ascii: bool,
    pub update_check: bool,
    pub git_timeout_ms: u64,
    pub validation_timeout_ms: u64,
    #[serde(alias = "provider_experimental")]
    pub agent_experimental: AgentExperimental,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentExperimental {
    pub codex_app_server: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            telemetry: false,
            retention_days: 30,
            ascii: false,
            update_check: false,
            git_timeout_ms: 2_000,
            validation_timeout_ms: 120_000,
            agent_experimental: AgentExperimental::default(),
        }
    }
}

impl AppConfig {
    pub fn read_or_create(paths: &AppPaths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            let config = Self::default();
            config.save(paths)?;
            return Ok(config);
        }
        if std::fs::symlink_metadata(&path)
            .at(&path)?
            .file_type()
            .is_symlink()
        {
            return Err(ContextWakeError::UnsafePath(format!(
                "configuration file is a symlink: {}",
                path.display()
            )));
        }
        let content = std::fs::read_to_string(&path).at(&path)?;
        let config: Self = toml::from_str(&content).map_err(|error| {
            ContextWakeError::Configuration(format!("{}: {error}", path.display()))
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).at(&path)?;
        let config: Self = toml::from_str(&raw)
            .map_err(|error| ContextWakeError::Configuration(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        self.validate()?;
        paths.ensure()?;
        let path = paths.config_file();
        if path.exists()
            && std::fs::symlink_metadata(&path)
                .at(&path)?
                .file_type()
                .is_symlink()
        {
            return Err(ContextWakeError::UnsafePath(format!(
                "refusing to replace configuration symlink {}",
                path.display()
            )));
        }
        let parent = path.parent().ok_or_else(|| {
            ContextWakeError::Configuration("configuration path has no parent".into())
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).at(parent)?;
        let raw = toml::to_string_pretty(self)
            .map_err(|error| ContextWakeError::Configuration(error.to_string()))?;
        temporary.write_all(raw.as_bytes()).at(&path)?;
        temporary.as_file_mut().sync_all().at(&path)?;
        temporary
            .persist(&path)
            .map_err(|error| ContextWakeError::Io {
                path: path.clone(),
                source: error.error,
            })?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(ContextWakeError::Configuration(format!(
                "unsupported config schema {}",
                self.schema_version
            )));
        }
        if self.retention_days == 0 || self.retention_days > 3_650 {
            return Err(ContextWakeError::Configuration(
                "retention_days must be between 1 and 3650".into(),
            ));
        }
        if !(100..=30_000).contains(&self.git_timeout_ms) {
            return Err(ContextWakeError::Configuration(
                "git_timeout_ms must be between 100 and 30000".into(),
            ));
        }
        if !(1_000..=1_800_000).contains(&self.validation_timeout_ms) {
            return Err(ContextWakeError::Configuration(
                "validation_timeout_ms must be between 1000 and 1800000".into(),
            ));
        }
        Ok(())
    }

    pub fn exists(paths: &AppPaths) -> bool {
        Path::new(&paths.config_file()).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_v1_config_receives_safe_new_defaults() {
        let config: AppConfig = toml::from_str(
            "schema_version = 1\ntelemetry = false\nretention_days = 30\nascii = false\nupdate_check = false\ngit_timeout_ms = 2000\n",
        )
        .expect("config");
        assert_eq!(config.validation_timeout_ms, 120_000);
        assert!(!config.telemetry);
        assert!(!config.update_check);
        config.validate().expect("valid");
    }

    #[test]
    fn legacy_provider_experimental_key_migrates_in_memory() {
        let config: AppConfig = toml::from_str(
            "schema_version = 1\ntelemetry = false\nretention_days = 30\nascii = false\nupdate_check = false\ngit_timeout_ms = 2000\nvalidation_timeout_ms = 120000\n[provider_experimental]\ncodex_app_server = true\n",
        )
        .expect("legacy config");
        assert!(config.agent_experimental.codex_app_server);
    }

    #[test]
    fn validation_timeout_is_bounded() {
        let config = AppConfig {
            validation_timeout_ms: 999,
            ..AppConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ContextWakeError::Configuration(_))
        ));
    }
}
