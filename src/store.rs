use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::error::{AgentDeckError, Result};
use crate::model::{
    AuthState, ContinuityKind, HandoffRecord, Profile, RedactionStatus, ResumeCapability, Session,
    TrustState, UsageSummary, Workspace,
};

const SCHEMA_VERSION: i64 = 4;

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_v2_to_v3(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    if !table_has_column(&transaction, "profiles", "agent_id")? {
        transaction.execute_batch(
            "ALTER TABLE profiles ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'codex';
             UPDATE profiles SET agent_id = provider_id;
             ALTER TABLE profiles ADD COLUMN model_provider_id TEXT;
             ALTER TABLE profiles ADD COLUMN model_preference TEXT;
             ALTER TABLE profiles ADD COLUMN agent_home TEXT;
             UPDATE profiles SET agent_home = provider_home;",
        )?;
    }
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS profiles_agent_home_idx ON profiles(agent_home);",
    )?;
    if !table_has_column(&transaction, "workspaces", "preferred_agent_id")? {
        transaction.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN preferred_agent_id TEXT;
             UPDATE workspaces SET preferred_agent_id = preferred_provider;
             ALTER TABLE workspaces ADD COLUMN preferred_model_provider_id TEXT;
             ALTER TABLE workspaces ADD COLUMN preferred_model TEXT;",
        )?;
    }
    if !table_has_column(&transaction, "sessions", "agent_id")? {
        transaction.execute_batch(
            "ALTER TABLE sessions ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'codex';
             UPDATE sessions SET agent_id = provider_id;
             ALTER TABLE sessions ADD COLUMN model_provider_id TEXT;",
        )?;
    }
    if !table_has_column(&transaction, "checkpoints", "agent_id")? {
        transaction.execute_batch(
            "ALTER TABLE checkpoints ADD COLUMN agent_id TEXT;
             UPDATE checkpoints SET agent_id = provider;
             ALTER TABLE checkpoints ADD COLUMN model_provider_id TEXT;
             ALTER TABLE checkpoints ADD COLUMN model TEXT;",
        )?;
    }
    transaction.execute("UPDATE schema_meta SET version = 3", [])?;
    transaction.commit()?;
    Ok(())
}

fn migrate_v3_to_v4(connection: &Connection) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    if !table_has_column(&transaction, "sessions", "title")? {
        transaction.execute("ALTER TABLE sessions ADD COLUMN title TEXT", [])?;
    }
    transaction.execute("UPDATE schema_meta SET version = 4", [])?;
    transaction.commit()?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { path: path.into() };
        if store.path.exists()
            && std::fs::symlink_metadata(&store.path)
                .map_err(|source| AgentDeckError::Io {
                    path: store.path.clone(),
                    source,
                })?
                .file_type()
                .is_symlink()
        {
            return Err(AgentDeckError::UnsafePath(format!(
                "state database is a symlink: {}",
                store.path.display()
            )));
        }
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AgentDeckError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let connection = self.connection()?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS schema_meta (
               version INTEGER NOT NULL
             );
             INSERT INTO schema_meta(version)
               SELECT 4 WHERE NOT EXISTS (SELECT 1 FROM schema_meta);
             CREATE TABLE IF NOT EXISTS profiles (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL COLLATE NOCASE UNIQUE,
               display_name TEXT NOT NULL,
               agent_id TEXT NOT NULL,
               model_provider_id TEXT,
               model_preference TEXT,
               description TEXT,
               agent_home TEXT NOT NULL UNIQUE,
               account_fingerprint TEXT,
               auth_state TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               last_used_at TEXT
             );
             CREATE TABLE IF NOT EXISTS settings (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS workspaces (
               id TEXT PRIMARY KEY,
               path TEXT NOT NULL UNIQUE,
               display_name TEXT NOT NULL,
               trust_state TEXT NOT NULL,
               preferred_agent_id TEXT,
               preferred_model_provider_id TEXT,
               preferred_model TEXT,
               preferred_profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
               last_session_id TEXT,
               git_root TEXT,
               created_at TEXT NOT NULL,
               last_opened_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY,
               provider_session_id TEXT,
               title TEXT,
               workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
               profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
               agent_id TEXT NOT NULL,
               model_provider_id TEXT,
               model TEXT,
               started_at TEXT NOT NULL,
               last_seen_at TEXT NOT NULL,
               resume_capability TEXT NOT NULL,
               archived INTEGER NOT NULL DEFAULT 0,
               continuity TEXT NOT NULL DEFAULT 'unknown'
             );
             CREATE INDEX IF NOT EXISTS sessions_workspace_idx
               ON sessions(workspace_id, last_seen_at DESC);
             CREATE TABLE IF NOT EXISTS checkpoints (
               id TEXT PRIMARY KEY,
               workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
               session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
               profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
               agent_id TEXT,
               model_provider_id TEXT,
               model TEXT,
               objective TEXT NOT NULL,
               storage_path TEXT NOT NULL UNIQUE,
               redaction_status TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS checkpoints_workspace_idx
               ON checkpoints(workspace_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS handoffs (
               id TEXT PRIMARY KEY,
               checkpoint_id TEXT NOT NULL,
               schema_version TEXT NOT NULL,
               storage_path TEXT NOT NULL UNIQUE,
               sha256 TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS usage_events (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               source TEXT NOT NULL,
               metric TEXT NOT NULL,
               value INTEGER NOT NULL,
               profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
               observed_at TEXT NOT NULL
             );",
        )?;
        let mut version: i64 =
            connection.query_row("SELECT version FROM schema_meta", [], |row| row.get(0))?;
        if version == 1 {
            connection.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 BEGIN IMMEDIATE;
                 ALTER TABLE handoffs RENAME TO handoffs_v1;
                 CREATE TABLE handoffs (
                   id TEXT PRIMARY KEY,
                   checkpoint_id TEXT NOT NULL,
                   schema_version TEXT NOT NULL,
                   storage_path TEXT NOT NULL UNIQUE,
                   sha256 TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 INSERT INTO handoffs
                   SELECT id, checkpoint_id, schema_version, storage_path, sha256, created_at
                   FROM handoffs_v1;
                 DROP TABLE handoffs_v1;
                 UPDATE schema_meta SET version = 2;
                 COMMIT;
                 PRAGMA foreign_keys = ON;",
            )?;
            version = 2;
        }
        if version == 2 {
            migrate_v2_to_v3(&connection)?;
            version = 3;
        }
        if version == 3 {
            migrate_v3_to_v4(&connection)?;
            version = 4;
        }
        if version != SCHEMA_VERSION {
            return Err(AgentDeckError::InvalidData(format!(
                "state schema {version} is not supported by this build"
            )));
        }
        Ok(())
    }

    pub fn integrity_check(&self) -> Result<String> {
        let connection = self.connection()?;
        Ok(connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?)
    }

    pub fn insert_profile(&self, profile: &Profile) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO profiles (
               id, name, display_name, agent_id, model_provider_id, model_preference,
               description, agent_home, account_fingerprint, auth_state, created_at,
               updated_at, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                profile.id.to_string(),
                profile.name,
                profile.display_name,
                profile.agent_id,
                profile.model_provider_id,
                profile.model_preference,
                profile.description,
                profile.agent_home.to_string_lossy(),
                profile.account_fingerprint,
                profile.auth_state.as_str(),
                profile.created_at.to_rfc3339(),
                profile.updated_at.to_rfc3339(),
                profile.last_used_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, name, display_name, agent_id, model_provider_id, model_preference,
                    description, agent_home, account_fingerprint, auth_state, created_at,
                    updated_at, last_used_at
             FROM profiles ORDER BY COALESCE(last_used_at, created_at) DESC, name",
        )?;
        let rows = statement.query_map([], profile_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn profile(&self, reference: &str) -> Result<Profile> {
        let connection = self.connection()?;
        let profile = connection
            .query_row(
                "SELECT id, name, display_name, agent_id, model_provider_id, model_preference,
                        description, agent_home, account_fingerprint, auth_state, created_at,
                        updated_at, last_used_at
                 FROM profiles WHERE id = ?1 OR name = ?1 COLLATE NOCASE",
                [reference],
                profile_from_row,
            )
            .optional()?;
        profile.ok_or_else(|| AgentDeckError::ProfileNotFound(reference.into()))
    }

    pub fn active_profile(&self) -> Result<Option<Profile>> {
        let connection = self.connection()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT value FROM settings WHERE key = 'active_profile_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|value| self.profile(&value)).transpose()
    }

    pub fn set_active_profile(&self, id: Uuid) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AgentDeckError::ProfileNotFound(id.to_string()));
        }
        let previous: Option<String> = transaction
            .query_row(
                "SELECT value FROM settings WHERE key = 'active_profile_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let id_string = id.to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO settings(key, value) VALUES ('active_profile_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&id_string],
        )?;
        transaction.execute(
            "UPDATE profiles SET last_used_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id_string],
        )?;
        if previous.is_some() && previous.as_deref() != Some(id_string.as_str()) {
            transaction.execute(
                "INSERT INTO usage_events(source, metric, value, profile_id, observed_at)
                 VALUES ('local', 'profile_switch', 1, ?1, ?2)",
                params![id_string, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_profile_auth(&self, id: Uuid, auth_state: AuthState) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE profiles SET auth_state = ?1, updated_at = ?2 WHERE id = ?3",
            params![auth_state.as_str(), Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn update_profile_model(
        &self,
        id: Uuid,
        model_provider_id: Option<&str>,
        model: Option<&str>,
    ) -> Result<Profile> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE profiles
             SET model_provider_id = ?1, model_preference = ?2, updated_at = ?3
             WHERE id = ?4",
            params![
                model_provider_id,
                model,
                Utc::now().to_rfc3339(),
                id.to_string()
            ],
        )?;
        if changed == 0 {
            return Err(AgentDeckError::ProfileNotFound(id.to_string()));
        }
        self.profile(&id.to_string())
    }

    pub fn remove_profile(&self, reference: &str) -> Result<Profile> {
        let profile = self.profile(reference)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM settings WHERE key = 'active_profile_id' AND value = ?1",
            [profile.id.to_string()],
        )?;
        transaction.execute(
            "DELETE FROM profiles WHERE id = ?1",
            [profile.id.to_string()],
        )?;
        transaction.commit()?;
        Ok(profile)
    }

    pub fn upsert_workspace(&self, workspace: &Workspace) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO workspaces (
               id, path, display_name, trust_state, preferred_agent_id,
               preferred_model_provider_id, preferred_model, preferred_profile_id,
               last_session_id, git_root, created_at, last_opened_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path) DO UPDATE SET
               display_name = excluded.display_name,
               trust_state = CASE
                 WHEN workspaces.trust_state = 'trusted' THEN 'trusted'
                 ELSE excluded.trust_state
               END,
               git_root = excluded.git_root,
               last_opened_at = excluded.last_opened_at",
            params![
                workspace.id.to_string(),
                workspace.path.to_string_lossy(),
                workspace.display_name,
                workspace.trust_state.as_str(),
                workspace.preferred_agent_id,
                workspace.preferred_model_provider_id,
                workspace.preferred_model,
                workspace
                    .preferred_profile_id
                    .map(|value| value.to_string()),
                workspace.last_session_id.map(|value| value.to_string()),
                workspace
                    .git_root
                    .as_ref()
                    .map(|value| value.to_string_lossy()),
                workspace.created_at.to_rfc3339(),
                workspace.last_opened_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, path, display_name, trust_state, preferred_agent_id,
                    preferred_model_provider_id, preferred_model, preferred_profile_id,
                    last_session_id, git_root, created_at, last_opened_at
             FROM workspaces ORDER BY last_opened_at DESC",
        )?;
        let rows = statement.query_map([], workspace_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn workspace(&self, reference: &str) -> Result<Workspace> {
        let connection = self.connection()?;
        let workspace = connection
            .query_row(
                "SELECT id, path, display_name, trust_state, preferred_agent_id,
                        preferred_model_provider_id, preferred_model, preferred_profile_id,
                        last_session_id, git_root, created_at, last_opened_at
                 FROM workspaces
                 WHERE id = ?1 OR path = ?1 OR display_name = ?1 COLLATE NOCASE",
                [reference],
                workspace_from_row,
            )
            .optional()?;
        workspace.ok_or_else(|| AgentDeckError::WorkspaceNotFound(reference.into()))
    }

    pub fn workspace_by_path(&self, path: &Path) -> Result<Option<Workspace>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, path, display_name, trust_state, preferred_agent_id,
                        preferred_model_provider_id, preferred_model, preferred_profile_id,
                        last_session_id, git_root, created_at, last_opened_at
                 FROM workspaces WHERE path = ?1",
                [path.to_string_lossy()],
                workspace_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn active_workspace(&self) -> Result<Option<Workspace>> {
        let connection = self.connection()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT value FROM settings WHERE key = 'active_workspace_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|value| self.workspace(&value)).transpose()
    }

    pub fn set_active_workspace(&self, id: Uuid) -> Result<()> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AgentDeckError::WorkspaceNotFound(id.to_string()));
        }
        connection.execute(
            "INSERT INTO settings(key, value) VALUES ('active_workspace_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [id.to_string()],
        )?;
        connection.execute(
            "UPDATE workspaces SET last_opened_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn remove_workspace(&self, reference: &str) -> Result<Workspace> {
        let workspace = self.workspace(reference)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM settings WHERE key = 'active_workspace_id' AND value = ?1",
            [workspace.id.to_string()],
        )?;
        transaction.execute(
            "DELETE FROM workspaces WHERE id = ?1",
            [workspace.id.to_string()],
        )?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO sessions (
               id, provider_session_id, title, workspace_id, profile_id, agent_id,
               model_provider_id, model, started_at, last_seen_at, resume_capability,
               archived, continuity
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                session.id.to_string(),
                session.provider_session_id,
                session.title,
                session.workspace_id.to_string(),
                session.profile_id.map(|value| value.to_string()),
                session.agent_id,
                session.model_provider_id,
                session.model,
                session.started_at.to_rfc3339(),
                session.last_seen_at.to_rfc3339(),
                session.resume_capability.as_str(),
                session.archived,
                session.continuity.as_str(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO usage_events(source, metric, value, profile_id, observed_at)
             VALUES ('local', 'session_count', 1, ?1, ?2)",
            params![
                session.profile_id.map(|value| value.to_string()),
                session.started_at.to_rfc3339()
            ],
        )?;
        transaction.execute(
            "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
            params![
                session.id.to_string(),
                session.last_seen_at.to_rfc3339(),
                session.workspace_id.to_string()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Inserts newly discovered provider metadata or refreshes an existing
    /// record owned by the same agent profile. Returns `true` when inserted.
    pub fn upsert_provider_session(&self, session: &Session) -> Result<bool> {
        let provider_session_id = session.provider_session_id.as_deref().ok_or_else(|| {
            AgentDeckError::InvalidData(
                "provider session synchronization requires a provider session ID".into(),
            )
        })?;
        let profile_id = session.profile_id.ok_or_else(|| {
            AgentDeckError::InvalidData(
                "provider session synchronization requires a profile".into(),
            )
        })?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM sessions
                 WHERE agent_id = ?1 AND profile_id = ?2 AND provider_session_id = ?3
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![
                    session.agent_id,
                    profile_id.to_string(),
                    provider_session_id
                ],
                |row| row.get(0),
            )
            .optional()?;
        let (inserted, persisted_id) = if let Some(id) = existing {
            transaction.execute(
                "UPDATE sessions SET title = ?1, workspace_id = ?2, model_provider_id = ?3, model = ?4,
                    started_at = ?5, last_seen_at = ?6, resume_capability = 'native', archived = 0
                 WHERE id = ?7",
                params![
                    session.title,
                    session.workspace_id.to_string(),
                    session.model_provider_id,
                    session.model,
                    session.started_at.to_rfc3339(),
                    session.last_seen_at.to_rfc3339(),
                    id,
                ],
            )?;
            (false, id)
        } else {
            transaction.execute(
                "INSERT INTO sessions (
                   id, provider_session_id, title, workspace_id, profile_id, agent_id,
                   model_provider_id, model, started_at, last_seen_at, resume_capability,
                   archived, continuity
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    session.id.to_string(),
                    provider_session_id,
                    session.title,
                    session.workspace_id.to_string(),
                    profile_id.to_string(),
                    session.agent_id,
                    session.model_provider_id,
                    session.model,
                    session.started_at.to_rfc3339(),
                    session.last_seen_at.to_rfc3339(),
                    session.resume_capability.as_str(),
                    i64::from(session.archived),
                    session.continuity.as_str(),
                ],
            )?;
            transaction.execute(
                "INSERT INTO usage_events(source, metric, value, profile_id, observed_at)
                 VALUES ('local', 'session_count', 1, ?1, ?2)",
                params![profile_id.to_string(), session.started_at.to_rfc3339()],
            )?;
            (true, session.id.to_string())
        };
        transaction.execute(
            "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
            params![
                persisted_id,
                session.last_seen_at.to_rfc3339(),
                session.workspace_id.to_string()
            ],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn record_native_resume(&self, reference: &str) -> Result<Session> {
        let session = self.session(reference)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions
             SET last_seen_at = ?1, archived = 0, resume_capability = 'native',
                 continuity = 'native_resume'
             WHERE id = ?2",
            params![now, session.id.to_string()],
        )?;
        transaction.execute(
            "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
            params![
                session.id.to_string(),
                now,
                session.workspace_id.to_string()
            ],
        )?;
        transaction.commit()?;
        self.session(&session.id.to_string())
    }

    pub fn list_sessions(&self, include_archived: bool) -> Result<Vec<Session>> {
        let connection = self.connection()?;
        let sql = if include_archived {
            "SELECT id, provider_session_id, title, workspace_id, profile_id, agent_id,
                    model_provider_id, model, started_at, last_seen_at, resume_capability,
                    archived, continuity
             FROM sessions ORDER BY last_seen_at DESC"
        } else {
            "SELECT id, provider_session_id, title, workspace_id, profile_id, agent_id,
                    model_provider_id, model, started_at, last_seen_at, resume_capability,
                    archived, continuity
             FROM sessions WHERE archived = 0 ORDER BY last_seen_at DESC"
        };
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([], session_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn session(&self, reference: &str) -> Result<Session> {
        let connection = self.connection()?;
        let session = connection
            .query_row(
                "SELECT id, provider_session_id, title, workspace_id, profile_id, agent_id,
                        model_provider_id, model, started_at, last_seen_at, resume_capability,
                        archived, continuity
                 FROM sessions WHERE id = ?1 OR provider_session_id = ?1",
                [reference],
                session_from_row,
            )
            .optional()?;
        session.ok_or_else(|| AgentDeckError::SessionNotFound(reference.into()))
    }

    pub fn archive_session(&self, reference: &str, archived: bool) -> Result<Session> {
        let session = self.session(reference)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions SET archived = ?1, last_seen_at = ?2 WHERE id = ?3",
            params![archived, now, session.id.to_string()],
        )?;
        if archived {
            transaction.execute(
                "UPDATE workspaces SET last_session_id = NULL WHERE last_session_id = ?1",
                [session.id.to_string()],
            )?;
        } else {
            transaction.execute(
                "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
                params![
                    session.id.to_string(),
                    now,
                    session.workspace_id.to_string()
                ],
            )?;
        }
        transaction.commit()?;
        self.session(&session.id.to_string())
    }

    pub fn insert_checkpoint_index(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        session_id: Option<Uuid>,
        profile_id: Option<Uuid>,
        agent_id: Option<&str>,
        model_provider_id: Option<&str>,
        model: Option<&str>,
        objective: &str,
        storage_path: &Path,
        redaction_status: RedactionStatus,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO checkpoints (
               id, workspace_id, session_id, profile_id, agent_id, model_provider_id,
               model, objective, storage_path, redaction_status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id.to_string(),
                workspace_id.to_string(),
                session_id.map(|value| value.to_string()),
                profile_id.map(|value| value.to_string()),
                agent_id,
                model_provider_id,
                model,
                objective,
                storage_path.to_string_lossy(),
                redaction_status.as_str(),
                created_at.to_rfc3339(),
            ],
        )?;
        connection.execute(
            "INSERT INTO usage_events(source, metric, value, profile_id, observed_at)
             VALUES ('local', 'checkpoint_count', 1, ?1, ?2)",
            params![
                profile_id.map(|value| value.to_string()),
                created_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn checkpoint_path(&self, reference: &str) -> Result<PathBuf> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT storage_path FROM checkpoints WHERE id = ?1",
                [reference],
                |row| row.get::<_, String>(0).map(PathBuf::from),
            )
            .optional()?
            .ok_or_else(|| AgentDeckError::CheckpointNotFound(reference.into()))
    }

    pub fn list_checkpoint_paths(&self) -> Result<Vec<(Uuid, String, PathBuf, DateTime<Utc>)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, objective, storage_path, created_at
             FROM checkpoints ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                parse_uuid(row.get::<_, String>(0)?)?,
                row.get(1)?,
                PathBuf::from(row.get::<_, String>(2)?),
                parse_time(row.get::<_, String>(3)?)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn delete_checkpoint_index(&self, reference: &str) -> Result<PathBuf> {
        let path = self.checkpoint_path(reference)?;
        let connection = self.connection()?;
        connection.execute("DELETE FROM checkpoints WHERE id = ?1", [reference])?;
        Ok(path)
    }

    pub fn insert_handoff(&self, record: &HandoffRecord) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO handoffs (
               id, checkpoint_id, schema_version, storage_path, sha256, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id.to_string(),
                record.checkpoint_id.to_string(),
                record.schema_version,
                record.storage_path.to_string_lossy(),
                record.sha256,
                record.created_at.to_rfc3339(),
            ],
        )?;
        connection.execute(
            "INSERT INTO usage_events(source, metric, value, observed_at)
             VALUES ('local', 'handoff_count', 1, ?1)",
            [record.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn handoff(&self, reference: &str) -> Result<HandoffRecord> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, checkpoint_id, schema_version, storage_path, sha256, created_at
                 FROM handoffs WHERE id = ?1",
                [reference],
                handoff_from_row,
            )
            .optional()?
            .ok_or_else(|| AgentDeckError::HandoffNotFound(reference.into()))
    }

    pub fn list_handoffs(&self) -> Result<Vec<HandoffRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, checkpoint_id, schema_version, storage_path, sha256, created_at
             FROM handoffs ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([], handoff_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn usage_summary(&self) -> Result<UsageSummary> {
        let connection = self.connection()?;
        let metric = |name: &str| -> rusqlite::Result<u64> {
            let value: i64 = connection.query_row(
                "SELECT COALESCE(SUM(value), 0) FROM usage_events
                 WHERE source = 'local' AND metric = ?1",
                [name],
                |row| row.get(0),
            )?;
            Ok(u64::try_from(value).unwrap_or_default())
        };
        Ok(UsageSummary {
            provider_usage_available: false,
            provider_usage_message:
                "Unavailable: no stable provider-usage interface is enabled by the active agent adapter"
                    .into(),
            local_session_count: metric("session_count")?,
            local_checkpoint_count: metric("checkpoint_count")?,
            local_handoff_count: metric("handoff_count")?,
            local_profile_switch_count: metric("profile_switch")?,
        })
    }
}

fn profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: parse_uuid(row.get(0)?)?,
        name: row.get(1)?,
        display_name: row.get(2)?,
        agent_id: row.get(3)?,
        model_provider_id: row.get(4)?,
        model_preference: row.get(5)?,
        description: row.get(6)?,
        agent_home: PathBuf::from(row.get::<_, String>(7)?),
        account_fingerprint: row.get(8)?,
        auth_state: AuthState::from(row.get::<_, String>(9)?.as_str()),
        created_at: parse_time(row.get(10)?)?,
        updated_at: parse_time(row.get(11)?)?,
        last_used_at: row
            .get::<_, Option<String>>(12)?
            .map(parse_time)
            .transpose()?,
    })
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: parse_uuid(row.get(0)?)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        display_name: row.get(2)?,
        trust_state: TrustState::from(row.get::<_, String>(3)?.as_str()),
        preferred_agent_id: row.get(4)?,
        preferred_model_provider_id: row.get(5)?,
        preferred_model: row.get(6)?,
        preferred_profile_id: row
            .get::<_, Option<String>>(7)?
            .map(parse_uuid)
            .transpose()?,
        last_session_id: row
            .get::<_, Option<String>>(8)?
            .map(parse_uuid)
            .transpose()?,
        git_root: row.get::<_, Option<String>>(9)?.map(PathBuf::from),
        created_at: parse_time(row.get(10)?)?,
        last_opened_at: parse_time(row.get(11)?)?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_uuid(row.get(0)?)?,
        provider_session_id: row.get(1)?,
        title: row.get(2)?,
        workspace_id: parse_uuid(row.get(3)?)?,
        profile_id: row
            .get::<_, Option<String>>(4)?
            .map(parse_uuid)
            .transpose()?,
        agent_id: row.get(5)?,
        model_provider_id: row.get(6)?,
        model: row.get(7)?,
        started_at: parse_time(row.get(8)?)?,
        last_seen_at: parse_time(row.get(9)?)?,
        resume_capability: ResumeCapability::from(row.get::<_, String>(10)?.as_str()),
        archived: row.get(11)?,
        continuity: ContinuityKind::from(row.get::<_, String>(12)?.as_str()),
    })
}

fn handoff_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HandoffRecord> {
    Ok(HandoffRecord {
        id: parse_uuid(row.get(0)?)?,
        checkpoint_id: parse_uuid(row.get(1)?)?,
        schema_version: row.get(2)?,
        storage_path: PathBuf::from(row.get::<_, String>(3)?),
        sha256: row.get(4)?,
        created_at: parse_time(row.get(5)?)?,
    })
}

fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                value.len(),
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_switch_is_transactional_and_recorded() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let now = Utc::now();
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "work".into(),
            display_name: "Work".into(),
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model_preference: None,
            description: None,
            agent_home: directory.path().join("codex-home"),
            account_fingerprint: None,
            auth_state: AuthState::Unknown,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        store.insert_profile(&profile).expect("insert profile");
        store.set_active_profile(profile.id).expect("activate");
        assert_eq!(
            store
                .usage_summary()
                .expect("usage")
                .local_profile_switch_count,
            0
        );
        let second = Profile {
            id: Uuid::new_v4(),
            name: "personal".into(),
            display_name: "Personal".into(),
            agent_home: directory.path().join("codex-home-personal"),
            ..profile.clone()
        };
        store
            .insert_profile(&second)
            .expect("insert second profile");
        store.set_active_profile(second.id).expect("switch");
        assert_eq!(
            store.active_profile().expect("active").unwrap().id,
            second.id
        );
        assert_eq!(
            store
                .usage_summary()
                .expect("usage")
                .local_profile_switch_count,
            1
        );
    }

    #[test]
    fn failed_switch_preserves_current_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let now = Utc::now();
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "personal".into(),
            display_name: "Personal".into(),
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model_preference: None,
            description: None,
            agent_home: directory.path().join("codex-home"),
            account_fingerprint: None,
            auth_state: AuthState::Unknown,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        store.insert_profile(&profile).expect("insert profile");
        store.set_active_profile(profile.id).expect("activate");
        assert!(store.set_active_profile(Uuid::new_v4()).is_err());
        assert_eq!(
            store.active_profile().expect("active").unwrap().id,
            profile.id
        );
    }

    #[test]
    fn session_activity_updates_workspace_pointer_transactionally() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let now = Utc::now();
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: directory.path().join("workspace"),
            display_name: "Workspace".into(),
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
        std::fs::create_dir_all(&workspace.path).expect("workspace directory");
        store.upsert_workspace(&workspace).expect("workspace");
        let session = Session {
            id: Uuid::new_v4(),
            provider_session_id: Some("provider-session".into()),
            title: None,
            workspace_id: workspace.id,
            profile_id: None,
            agent_id: "codex".into(),
            model_provider_id: Some("openai".into()),
            model: None,
            started_at: now,
            last_seen_at: now,
            resume_capability: ResumeCapability::Unknown,
            archived: false,
            continuity: ContinuityKind::NewSession,
        };
        store.insert_session(&session).expect("session");
        assert_eq!(
            store
                .workspace(&workspace.id.to_string())
                .unwrap()
                .last_session_id,
            Some(session.id)
        );

        let archived = store
            .archive_session(&session.id.to_string(), true)
            .expect("archive");
        assert!(archived.archived);
        assert_eq!(
            store
                .workspace(&workspace.id.to_string())
                .unwrap()
                .last_session_id,
            None
        );

        let resumed = store
            .record_native_resume(&session.id.to_string())
            .expect("resume");
        assert!(!resumed.archived);
        assert_eq!(resumed.continuity, ContinuityKind::NativeResume);
        assert_eq!(
            store
                .workspace(&workspace.id.to_string())
                .unwrap()
                .last_session_id,
            Some(session.id)
        );
        assert_eq!(
            store.usage_summary().unwrap().local_session_count,
            1,
            "resume is activity, not a newly created local session"
        );
    }

    #[test]
    fn provider_session_sync_is_idempotent_per_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let now = Utc::now();
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "oss".into(),
            display_name: "OSS".into(),
            agent_id: "opencode".into(),
            model_provider_id: Some("opencode".into()),
            model_preference: Some("big-pickle".into()),
            description: None,
            agent_home: directory.path().join("opencode-home"),
            account_fingerprint: None,
            auth_state: AuthState::SignedOut,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        };
        store.insert_profile(&profile).expect("profile");
        let workspace = Workspace {
            id: Uuid::new_v4(),
            path: directory.path().join("workspace"),
            display_name: "Workspace".into(),
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
        std::fs::create_dir_all(&workspace.path).expect("workspace directory");
        store.upsert_workspace(&workspace).expect("workspace");
        let session = Session {
            id: Uuid::new_v4(),
            provider_session_id: Some("ses_123".into()),
            title: Some("Implement sync".into()),
            workspace_id: workspace.id,
            profile_id: Some(profile.id),
            agent_id: "opencode".into(),
            model_provider_id: Some("opencode".into()),
            model: Some("big-pickle".into()),
            started_at: now,
            last_seen_at: now,
            resume_capability: ResumeCapability::Native,
            archived: false,
            continuity: ContinuityKind::Unknown,
        };
        assert!(store.upsert_provider_session(&session).expect("insert"));
        let refreshed = Session {
            id: Uuid::new_v4(),
            title: Some("Implement sync safely".into()),
            last_seen_at: now + chrono::Duration::minutes(1),
            ..session
        };
        assert!(!store.upsert_provider_session(&refreshed).expect("update"));
        let sessions = store.list_sessions(false).expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider_session_id.as_deref(), Some("ses_123"));
        assert_eq!(sessions[0].title.as_deref(), Some("Implement sync safely"));
        assert_eq!(sessions[0].last_seen_at, refreshed.last_seen_at);
        assert_eq!(
            store.usage_summary().unwrap().local_session_count,
            1,
            "refreshing discovery must not count as a new session"
        );
    }

    #[test]
    fn migrates_v1_handoffs_to_support_imported_artifacts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("legacy database");
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (1);
                 CREATE TABLE handoffs (
                   id TEXT PRIMARY KEY,
                   checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
                   schema_version TEXT NOT NULL,
                   storage_path TEXT NOT NULL UNIQUE,
                   sha256 TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );",
            )
            .expect("legacy schema");
        drop(connection);

        let store = Store::open(&path).expect("migration");
        let record = HandoffRecord {
            id: Uuid::new_v4(),
            checkpoint_id: Uuid::new_v4(),
            schema_version: "1.0.0".into(),
            storage_path: directory.path().join("imported"),
            sha256: "0".repeat(64),
            created_at: Utc::now(),
        };
        store
            .insert_handoff(&record)
            .expect("orphan imported handoff is supported");
        assert_eq!(
            store.handoff(&record.id.to_string()).expect("record"),
            record
        );
    }

    #[test]
    fn migrates_v2_provider_fields_without_losing_records() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let profile_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let checkpoint_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        let profile_home = directory.path().join("legacy-claude-home");
        let workspace_path = directory.path().join("workspace");
        let checkpoint_path = directory.path().join("checkpoint.json");
        let connection = Connection::open(&path).expect("legacy database");
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (2);
                 CREATE TABLE profiles (
                   id TEXT PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                   display_name TEXT NOT NULL, provider_id TEXT NOT NULL,
                   description TEXT, provider_home TEXT NOT NULL UNIQUE,
                   account_fingerprint TEXT, auth_state TEXT NOT NULL,
                   created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_used_at TEXT
                 );
                 CREATE TABLE workspaces (
                   id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, display_name TEXT NOT NULL,
                   trust_state TEXT NOT NULL, preferred_provider TEXT,
                   preferred_profile_id TEXT, last_session_id TEXT, git_root TEXT,
                   created_at TEXT NOT NULL, last_opened_at TEXT NOT NULL
                 );
                 CREATE TABLE sessions (
                   id TEXT PRIMARY KEY, provider_session_id TEXT, workspace_id TEXT NOT NULL,
                   profile_id TEXT, provider_id TEXT NOT NULL, model TEXT,
                   started_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                   resume_capability TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
                   continuity TEXT NOT NULL DEFAULT 'unknown'
                 );
                 CREATE TABLE checkpoints (
                   id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, session_id TEXT,
                   profile_id TEXT, provider TEXT, objective TEXT NOT NULL,
                   storage_path TEXT NOT NULL UNIQUE, redaction_status TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );",
            )
            .expect("legacy schema");
        connection
            .execute(
                "INSERT INTO profiles VALUES (?1, 'work', 'Work', 'claude', NULL, ?2, NULL, 'signed_out', ?3, ?3, NULL)",
                params![profile_id.to_string(), profile_home.to_string_lossy(), now],
            )
            .expect("legacy profile");
        connection
            .execute(
                "INSERT INTO workspaces VALUES (?1, ?2, 'Workspace', 'untrusted', 'claude', ?3, ?4, NULL, ?5, ?5)",
                params![
                    workspace_id.to_string(),
                    workspace_path.to_string_lossy(),
                    profile_id.to_string(),
                    session_id.to_string(),
                    now
                ],
            )
            .expect("legacy workspace");
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, 'claude-session', ?2, ?3, 'claude', 'sonnet', ?4, ?4, 'native', 0, 'new_session')",
                params![
                    session_id.to_string(),
                    workspace_id.to_string(),
                    profile_id.to_string(),
                    now
                ],
            )
            .expect("legacy session");
        connection
            .execute(
                "INSERT INTO checkpoints VALUES (?1, ?2, ?3, ?4, 'claude', 'Continue', ?5, 'clean', ?6)",
                params![
                    checkpoint_id.to_string(),
                    workspace_id.to_string(),
                    session_id.to_string(),
                    profile_id.to_string(),
                    checkpoint_path.to_string_lossy(),
                    now
                ],
            )
            .expect("legacy checkpoint");
        drop(connection);

        let store = Store::open(&path).expect("migration");
        let profile = store.profile("work").expect("migrated profile");
        assert_eq!(profile.agent_id, "claude");
        assert_eq!(profile.agent_home, profile_home);
        assert_eq!(profile.model_provider_id, None);
        let workspace = store
            .workspace(&workspace_id.to_string())
            .expect("migrated workspace");
        assert_eq!(workspace.preferred_agent_id.as_deref(), Some("claude"));
        let session = store
            .session(&session_id.to_string())
            .expect("migrated session");
        assert_eq!(session.agent_id, "claude");
        assert_eq!(session.model.as_deref(), Some("sonnet"));
        assert_eq!(session.model_provider_id, None);
        assert_eq!(session.title, None);

        let connection = store.connection().expect("connection");
        let migrated_checkpoint_agent: Option<String> = connection
            .query_row(
                "SELECT agent_id FROM checkpoints WHERE id = ?1",
                [checkpoint_id.to_string()],
                |row| row.get(0),
            )
            .expect("migrated checkpoint");
        assert_eq!(migrated_checkpoint_agent.as_deref(), Some("claude"));
        let version: i64 = connection
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 4);
    }
}
