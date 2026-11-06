use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ContextWakeError, IoContext, Result};
use crate::paths::AppPaths;

/// Global settings from `config.toml`.
///
/// `telemetry`, `update_check`, `retention_days` and
/// `agent_experimental.codex_app_server` are reserved: they are parsed so that
/// existing files keep loading, but no feature reads them yet. `ContextWake`
/// sends no telemetry, never checks for updates, never deletes checkpoints,
/// handoffs or session records on its own, and has no Codex app-server
/// integration. [`AppConfig::save`] refuses to switch them on, and
/// [`AppConfig::reserved_setting_notices`] reports any that a file sets.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent on/off settings of a configuration file"
)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub schema_version: u32,
    /// Reserved; no effect.
    pub telemetry: bool,
    /// Reserved; no effect. Nothing is deleted automatically.
    pub retention_days: u32,
    pub ascii: bool,
    /// Reserved; no effect.
    pub update_check: bool,
    pub git_timeout_ms: u64,
    pub validation_timeout_ms: u64,
    /// Whether checkpoints (including switch checkpoints) store a bounded,
    /// redacted excerpt of the source agent's recent conversation in the
    /// workspace, read from that agent's local transcripts.
    pub capture_conversation_excerpts: bool,
    #[serde(alias = "provider_experimental")]
    pub agent_experimental: AgentExperimental,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentExperimental {
    /// Reserved; no effect.
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
            capture_conversation_excerpts: true,
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

    /// Validates and atomically writes the configuration. Switching on a
    /// reserved setting is refused (see [`AppConfig`]); values already stored
    /// in the file are kept, so other settings stay changeable.
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
        let persisted = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| toml::from_str::<Self>(&raw).ok())
            .unwrap_or_default();
        self.refuse_enabling_reserved_settings(&persisted)?;
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

    /// Explains each reserved setting that is set to something other than its
    /// default. None of them has an effect yet, so a non-default value is
    /// reported instead of being silently ignored.
    pub fn reserved_setting_notices(&self) -> Vec<String> {
        let defaults = Self::default();
        let mut notices = Vec::new();
        if self.telemetry != defaults.telemetry {
            notices.push(
                "telemetry = true has no effect: ContextWake does not collect or send telemetry"
                    .to_owned(),
            );
        }
        if self.update_check != defaults.update_check {
            notices.push(
                "update_check = true has no effect: ContextWake does not check for updates"
                    .to_owned(),
            );
        }
        if self.retention_days != defaults.retention_days {
            notices.push(format!(
                "retention_days = {} has no effect: ContextWake never deletes checkpoints, handoffs or session records automatically; delete them explicitly, for example with 'ctx checkpoint delete <id> --yes'",
                self.retention_days
            ));
        }
        if self.agent_experimental.codex_app_server != defaults.agent_experimental.codex_app_server
        {
            notices.push(
                "agent_experimental.codex_app_server = true has no effect: no Codex app-server integration exists"
                    .to_owned(),
            );
        }
        notices
    }

    fn refuse_enabling_reserved_settings(&self, persisted: &Self) -> Result<()> {
        let defaults = Self::default();
        let mut refused = Vec::new();
        if self.telemetry != persisted.telemetry && self.telemetry != defaults.telemetry {
            refused.push("telemetry");
        }
        if self.update_check != persisted.update_check && self.update_check != defaults.update_check
        {
            refused.push("update_check");
        }
        if self.retention_days != persisted.retention_days
            && self.retention_days != defaults.retention_days
        {
            refused.push("retention_days");
        }
        let codex_app_server = self.agent_experimental.codex_app_server;
        if codex_app_server != persisted.agent_experimental.codex_app_server
            && codex_app_server != defaults.agent_experimental.codex_app_server
        {
            refused.push("agent_experimental.codex_app_server");
        }
        if refused.is_empty() {
            return Ok(());
        }
        Err(ContextWakeError::Configuration(format!(
            "refusing to change {}: reserved settings have no effect yet. ContextWake sends no telemetry, never checks for updates, never deletes checkpoints, handoffs or session records automatically (delete them with 'ctx checkpoint delete <id> --yes'), and has no Codex app-server integration.",
            refused.join(", ")
        )))
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
    fn reserved_settings_cannot_be_switched_on() {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path());
        AppConfig::read_or_create(&paths).expect("defaults");
        for change in [
            AppConfig {
                telemetry: true,
                ..AppConfig::default()
            },
            AppConfig {
                update_check: true,
                ..AppConfig::default()
            },
            AppConfig {
                retention_days: 7,
                ..AppConfig::default()
            },
            AppConfig {
                agent_experimental: AgentExperimental {
                    codex_app_server: true,
                },
                ..AppConfig::default()
            },
        ] {
            assert!(
                matches!(
                    change.save(&paths),
                    Err(ContextWakeError::Configuration(message)) if message.contains("reserved")
                ),
                "{change:?}"
            );
        }
        assert_eq!(
            AppConfig::read_or_create(&paths).expect("reload"),
            AppConfig::default(),
            "a refused change writes nothing"
        );
        AppConfig {
            ascii: true,
            ..AppConfig::default()
        }
        .save(&paths)
        .expect("implemented settings still change");
    }

    #[test]
    fn stored_reserved_values_are_reported_without_blocking_other_changes() {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path());
        paths.ensure().expect("paths");
        std::fs::write(
            paths.config_file(),
            "schema_version = 1\ntelemetry = true\nretention_days = 90\n",
        )
        .expect("existing config");
        let config = AppConfig::read_or_create(&paths).expect("existing file still loads");
        let notices = config.reserved_setting_notices();
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("retention_days = 90"))
        );
        AppConfig {
            ascii: true,
            ..config.clone()
        }
        .save(&paths)
        .expect("other settings stay changeable");
        AppConfig {
            telemetry: false,
            retention_days: 30,
            ..config
        }
        .save(&paths)
        .expect("reserved settings can be reset");
        assert!(AppConfig::default().reserved_setting_notices().is_empty());
    }

    #[test]
    fn conversation_excerpts_are_captured_unless_switched_off() {
        let older: AppConfig = toml::from_str("schema_version = 1\n").expect("older config");
        assert!(older.capture_conversation_excerpts);
        let off: AppConfig =
            toml::from_str("schema_version = 1\ncapture_conversation_excerpts = false\n")
                .expect("config");
        assert!(!off.capture_conversation_excerpts);
        assert!(off.reserved_setting_notices().is_empty());

        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::from_root(root.path());
        off.save(&paths).expect("not a reserved setting");
        assert!(
            !AppConfig::read_or_create(&paths)
                .expect("reload")
                .capture_conversation_excerpts
        );
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
