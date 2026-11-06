use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

use crate::error::{ContextWakeError, Result};
use crate::model::{
    AuthState, ContinuityKind, HandoffRecord, Profile, RedactionStatus, ResumeCapability, Session,
    TrustState, UsageSummary, Workspace,
};
use crate::paths::{
    AGENT_HOMES_DIR, CHECKPOINTS_DIR, HANDOFFS_DIR, canonicalize_simplified, paths_equivalent,
    simplify_path,
};

const SCHEMA_VERSION: i64 = 5;

/// Expands to a `SELECT` of every profile column followed by `$tail`.
macro_rules! select_profiles {
    ($tail:literal) => {
        concat!(
            "SELECT id, name, display_name, agent_id, model_provider_id, model_preference,
                    description, agent_home, account_fingerprint, auth_state, created_at,
                    updated_at, last_used_at
             FROM profiles ",
            $tail
        )
    };
}

/// Expands to a `SELECT` of every workspace column followed by `$tail`.
macro_rules! select_workspaces {
    ($tail:literal) => {
        concat!(
            "SELECT id, path, display_name, trust_state, preferred_agent_id,
                    preferred_model_provider_id, preferred_model, preferred_profile_id,
                    last_session_id, git_root, created_at, last_opened_at
             FROM workspaces ",
            $tail
        )
    };
}

/// Expands to a `SELECT` of every session column followed by `$tail`.
macro_rules! select_sessions {
    ($tail:literal) => {
        concat!(
            "SELECT id, provider_session_id, title, workspace_id, profile_id, agent_id,
                    model_provider_id, model, started_at, last_seen_at, resume_capability,
                    archived, continuity
             FROM sessions ",
            $tail
        )
    };
}

/// Current table definitions. Every statement is idempotent, so the batch runs
/// on each open before the versioned migrations bring older tables up to date.
/// A new database records version 4 and then takes the same idempotent v4 to
/// v5 step as an upgraded one.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS schema_meta (
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
     );
     CREATE TABLE IF NOT EXISTS workspace_aliases (
       alias_id TEXT PRIMARY KEY,
       workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE
     );";

/// Path columns that hold entries inside the data directory.
const MANAGED_PATH_COLUMNS: [(&str, &str); 3] = [
    (
        "SELECT id, agent_home FROM profiles WHERE agent_home IS NOT NULL",
        "UPDATE OR IGNORE profiles SET agent_home = ?1 WHERE id = ?2",
    ),
    (
        "SELECT id, storage_path FROM checkpoints WHERE storage_path IS NOT NULL",
        "UPDATE OR IGNORE checkpoints SET storage_path = ?1 WHERE id = ?2",
    ),
    (
        "SELECT id, storage_path FROM handoffs WHERE storage_path IS NOT NULL",
        "UPDATE OR IGNORE handoffs SET storage_path = ?1 WHERE id = ?2",
    ),
];

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

/// Opens a transaction that takes the write lock up front, where the busy
/// timeout applies. A deferred transaction that reads first and writes later
/// fails at once with "database is locked" while another connection writes.
fn write_transaction(connection: &mut Connection) -> Result<Transaction<'_>> {
    Ok(connection.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

fn unsupported_schema(version: i64) -> ContextWakeError {
    ContextWakeError::InvalidData(format!(
        "state schema {version} is not supported by this build"
    ))
}

/// Reads the recorded schema version without changing the database. `None`
/// means no version is recorded yet (a new database).
fn recorded_schema_version(connection: &Connection) -> Result<Option<i64>> {
    let has_metadata: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_metadata {
        return Ok(None);
    }
    let rows: i64 =
        connection.query_row("SELECT COUNT(*) FROM schema_meta", [], |row| row.get(0))?;
    match rows {
        0 => Ok(None),
        1 => Ok(Some(connection.query_row(
            "SELECT version FROM schema_meta",
            [],
            |row| row.get(0),
        )?)),
        _ => Err(ContextWakeError::InvalidData(format!(
            "state schema metadata must contain exactly one row, found {rows}"
        ))),
    }
}

fn schema_version(connection: &Connection) -> Result<i64> {
    recorded_schema_version(connection)?.ok_or_else(|| {
        ContextWakeError::InvalidData(
            "state schema metadata must contain exactly one row, found 0".into(),
        )
    })
}

fn migrate_v1_to_v2(connection: &Connection) -> Result<()> {
    // v1 cascaded handoffs from checkpoints; imported handoffs have no local
    // checkpoint, so the table is rebuilt without that foreign key.
    connection.execute_batch(
        "ALTER TABLE handoffs RENAME TO handoffs_v1;
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
         UPDATE schema_meta SET version = 2;",
    )?;
    Ok(())
}

fn migrate_v2_to_v3(connection: &Connection) -> Result<()> {
    if !table_has_column(connection, "profiles", "agent_id")? {
        connection.execute_batch(
            "ALTER TABLE profiles ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'codex';
             UPDATE profiles SET agent_id = provider_id;
             ALTER TABLE profiles ADD COLUMN model_provider_id TEXT;
             ALTER TABLE profiles ADD COLUMN model_preference TEXT;
             ALTER TABLE profiles ADD COLUMN agent_home TEXT;
             UPDATE profiles SET agent_home = provider_home;",
        )?;
    }
    connection.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS profiles_agent_home_idx ON profiles(agent_home);",
    )?;
    if !table_has_column(connection, "workspaces", "preferred_agent_id")? {
        connection.execute_batch(
            "ALTER TABLE workspaces ADD COLUMN preferred_agent_id TEXT;
             UPDATE workspaces SET preferred_agent_id = preferred_provider;
             ALTER TABLE workspaces ADD COLUMN preferred_model_provider_id TEXT;
             ALTER TABLE workspaces ADD COLUMN preferred_model TEXT;",
        )?;
    }
    if !table_has_column(connection, "sessions", "agent_id")? {
        connection.execute_batch(
            "ALTER TABLE sessions ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'codex';
             UPDATE sessions SET agent_id = provider_id;
             ALTER TABLE sessions ADD COLUMN model_provider_id TEXT;",
        )?;
    }
    if !table_has_column(connection, "checkpoints", "agent_id")? {
        connection.execute_batch(
            "ALTER TABLE checkpoints ADD COLUMN agent_id TEXT;
             UPDATE checkpoints SET agent_id = provider;
             ALTER TABLE checkpoints ADD COLUMN model_provider_id TEXT;
             ALTER TABLE checkpoints ADD COLUMN model TEXT;",
        )?;
    }
    connection.execute("UPDATE schema_meta SET version = 3", [])?;
    Ok(())
}

fn migrate_v3_to_v4(connection: &Connection) -> Result<()> {
    if !table_has_column(connection, "sessions", "title")? {
        connection.execute("ALTER TABLE sessions ADD COLUMN title TEXT", [])?;
    }
    connection.execute("UPDATE schema_meta SET version = 4", [])?;
    Ok(())
}

/// v5 stores paths in their conventional form (Windows builds before v5
/// recorded canonical `\\?\` verbatim workspace paths) and repairs databases
/// that the v2 to v3 step left with NOT NULL legacy columns.
fn migrate_v4_to_v5(connection: &Connection) -> Result<()> {
    repair_legacy_agent_columns(connection)?;
    normalize_workspace_paths(connection)?;
    for (select, update) in std::iter::once((
        "SELECT id, git_root FROM workspaces WHERE git_root IS NOT NULL",
        "UPDATE workspaces SET git_root = ?1 WHERE id = ?2",
    ))
    .chain(MANAGED_PATH_COLUMNS)
    {
        rewrite_stored_paths(connection, select, update, |stored| {
            let conventional = stored_path(stored);
            (conventional.as_os_str() != stored.as_os_str()).then_some(conventional)
        })?;
    }
    connection.execute("UPDATE schema_meta SET version = 5", [])?;
    Ok(())
}

/// Rebuilds `profiles` and `sessions` without their pre-v3 `provider_*`
/// columns. The v2 to v3 step added the new columns next to the old ones, and
/// the old NOT NULL columns (which have no default) rejected every new profile
/// and session. The caller has disabled foreign keys, so dropping the old
/// table does not clear references to its rows.
fn repair_legacy_agent_columns(connection: &Connection) -> Result<()> {
    if table_has_column(connection, "profiles", "provider_id")?
        || table_has_column(connection, "profiles", "provider_home")?
    {
        connection.execute_batch(
            "CREATE TABLE profiles_v5 (
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
             INSERT INTO profiles_v5 (
               id, name, display_name, agent_id, model_provider_id, model_preference,
               description, agent_home, account_fingerprint, auth_state, created_at,
               updated_at, last_used_at
             )
             SELECT id, name, display_name, agent_id, model_provider_id, model_preference,
                    description, agent_home, account_fingerprint, auth_state, created_at,
                    updated_at, last_used_at
             FROM profiles;
             DROP TABLE profiles;
             ALTER TABLE profiles_v5 RENAME TO profiles;",
        )?;
    }
    if table_has_column(connection, "sessions", "provider_id")? {
        connection.execute_batch(
            "CREATE TABLE sessions_v5 (
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
             INSERT INTO sessions_v5 (
               id, provider_session_id, title, workspace_id, profile_id, agent_id,
               model_provider_id, model, started_at, last_seen_at, resume_capability,
               archived, continuity
             )
             SELECT id, provider_session_id, title, workspace_id, profile_id, agent_id,
                    model_provider_id, model, started_at, last_seen_at, resume_capability,
                    archived, continuity
             FROM sessions;
             DROP TABLE sessions;
             ALTER TABLE sessions_v5 RENAME TO sessions;
             CREATE INDEX IF NOT EXISTS sessions_workspace_idx
               ON sessions(workspace_id, last_seen_at DESC);",
        )?;
    }
    Ok(())
}

/// Stores every workspace path in its conventional form. Rows whose paths
/// name the same directory (a verbatim `\\?\` spelling next to the
/// conventional one) are merged into the oldest registration. The merged IDs
/// stay resolvable as aliases because checkpoint files record them.
fn normalize_workspace_paths(connection: &Connection) -> Result<()> {
    let rows = {
        let mut statement =
            connection.prepare("SELECT id, path FROM workspaces ORDER BY created_at, id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (id, path) in rows {
        groups
            .entry(path_merge_key(Path::new(&path)))
            .or_default()
            .push((id, path));
    }
    for members in groups.values() {
        let (survivor, survivor_path) = &members[0];
        for (duplicate, _) in &members[1..] {
            merge_workspace_into(connection, survivor, duplicate)?;
        }
        // Prefer the spelling of a canonical (verbatim) path, which carries the
        // directory's real letter case.
        let canonical = members
            .iter()
            .map(|(_, path)| path)
            .find(|path| simplify_path(Path::new(path)).as_os_str() != OsStr::new(path.as_str()))
            .unwrap_or(survivor_path);
        let target = stored_path(Path::new(canonical));
        if target.as_os_str() != OsStr::new(survivor_path.as_str()) {
            connection.execute(
                "UPDATE workspaces SET path = ?1 WHERE id = ?2",
                params![target.to_string_lossy(), survivor],
            )?;
        }
    }
    Ok(())
}

/// The spelling a workspace path is stored in: the conventional form (see
/// [`simplify_path`]) with the platform separator and without `.` components or
/// trailing separators.
fn stored_path(path: &Path) -> PathBuf {
    simplify_path(path).components().collect()
}

/// Groups stored paths that differ only in spelling that cannot change the
/// directory they name: the verbatim prefix, and on Windows `/` versus `\` and
/// trailing separators. Letter case is kept on purpose: a directory can be
/// case-sensitive on Windows, so such rows stay separate (and a lookup that
/// matches both is refused as ambiguous).
fn path_merge_key(path: &Path) -> String {
    let key = simplify_path(path).to_string_lossy().into_owned();
    if cfg!(windows) {
        key.replace('/', "\\").trim_end_matches('\\').to_owned()
    } else {
        key
    }
}

/// Moves everything that references `duplicate` to `survivor`, keeps the
/// user's trust decision and the newest activity, records `duplicate` as an
/// alias and deletes the duplicate row.
fn merge_workspace_into(connection: &Connection, survivor: &str, duplicate: &str) -> Result<()> {
    for statement in [
        "UPDATE sessions SET workspace_id = ?1 WHERE workspace_id = ?2",
        "UPDATE checkpoints SET workspace_id = ?1 WHERE workspace_id = ?2",
        "UPDATE settings SET value = ?1 WHERE key = 'active_workspace_id' AND value = ?2",
        "UPDATE workspace_aliases SET workspace_id = ?1 WHERE workspace_id = ?2",
        "INSERT OR IGNORE INTO workspace_aliases(alias_id, workspace_id) VALUES (?2, ?1)",
        "UPDATE workspaces SET
           trust_state = CASE
             WHEN workspaces.trust_state = 'trusted' OR duplicate.trust_state = 'trusted'
               THEN 'trusted'
             ELSE workspaces.trust_state
           END,
           preferred_agent_id = COALESCE(workspaces.preferred_agent_id, duplicate.preferred_agent_id),
           preferred_model_provider_id =
             COALESCE(workspaces.preferred_model_provider_id, duplicate.preferred_model_provider_id),
           preferred_model = COALESCE(workspaces.preferred_model, duplicate.preferred_model),
           preferred_profile_id =
             COALESCE(workspaces.preferred_profile_id, duplicate.preferred_profile_id),
           last_session_id = COALESCE(workspaces.last_session_id, duplicate.last_session_id),
           git_root = COALESCE(workspaces.git_root, duplicate.git_root),
           created_at = MIN(workspaces.created_at, duplicate.created_at),
           last_opened_at = MAX(workspaces.last_opened_at, duplicate.last_opened_at)
         FROM workspaces AS duplicate
         WHERE workspaces.id = ?1 AND duplicate.id = ?2",
        "DELETE FROM workspaces WHERE id = ?2",
    ] {
        connection.execute(statement, params![survivor, duplicate])?;
    }
    Ok(())
}

/// Applies `rewrite` to the `(id, path)` rows returned by `select` and stores
/// each changed path with `update`. Returns the number of rewritten rows.
fn rewrite_stored_paths(
    connection: &Connection,
    select: &str,
    update: &str,
    rewrite: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<usize> {
    let rows = {
        let mut statement = connection.prepare(select)?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut rewritten = 0;
    for (id, stored) in rows {
        if let Some(path) = rewrite(Path::new(&stored)) {
            rewritten += connection.execute(update, params![path.to_string_lossy(), id])?;
        }
    }
    Ok(rewritten)
}

/// Builds a managed entry's path from the data directory, the row ID and a
/// qualifier (the agent ID for agent homes).
type ManagedEntry = fn(&Path, &str, &str) -> PathBuf;

fn agent_home_entry(data_dir: &Path, profile_id: &str, agent_id: &str) -> PathBuf {
    data_dir
        .join(AGENT_HOMES_DIR)
        .join(agent_id)
        .join(profile_id)
}

fn checkpoint_entry(data_dir: &Path, checkpoint_id: &str, _: &str) -> PathBuf {
    data_dir
        .join(CHECKPOINTS_DIR)
        .join(format!("{checkpoint_id}.json"))
}

fn handoff_entry(data_dir: &Path, handoff_id: &str, _: &str) -> PathBuf {
    data_dir.join(HANDOFFS_DIR).join(handoff_id)
}

/// True when `stored` names the managed entry `expected` (the same last
/// `depth` components) inside another directory, and that entry exists here.
fn is_relocated_entry(stored: &Path, expected: &Path, depth: usize) -> bool {
    let tail = |path: &Path| {
        path.components()
            .rev()
            .take(depth)
            .map(|component| component.as_os_str().to_os_string())
            .collect::<Vec<_>>()
    };
    let stored_tail = tail(stored);
    !paths_equivalent(stored, expected)
        && stored_tail.len() == depth
        && stored_tail == tail(expected)
        && std::fs::symlink_metadata(expected).is_ok()
}

/// What removing a workspace's metadata deletes from the state database.
/// Repository files and checkpoint files are never deleted: the files listed
/// in `checkpoint_files` stay on disk without an index entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceRemoval {
    pub workspace: Workspace,
    pub session_records: usize,
    pub checkpoint_records: usize,
    pub checkpoint_files: Vec<PathBuf>,
}

/// One row of the checkpoint index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckpointIndexEntry {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub objective: String,
    pub storage_path: PathBuf,
    pub created_at: DateTime<Utc>,
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
                .map_err(|source| ContextWakeError::Io {
                    path: store.path.clone(),
                    source,
                })?
                .file_type()
                .is_symlink()
        {
            return Err(ContextWakeError::UnsafePath(format!(
                "state database is a symlink: {}",
                store.path.display()
            )));
        }
        store.migrate()?;
        store.relocate_managed_paths()?;
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
            std::fs::create_dir_all(parent).map_err(|source| ContextWakeError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut connection = self.connection()?;
        // Rebuilding a table must not fire the ON DELETE actions of the tables
        // that reference it. SQLite ignores this pragma inside a transaction,
        // so it is switched off for the whole (private) migration connection.
        connection.pragma_update(None, "foreign_keys", "OFF")?;
        // A database written by a newer build is refused before anything in
        // it is changed.
        if let Some(version) = recorded_schema_version(&connection)?
            && !(1..=SCHEMA_VERSION).contains(&version)
        {
            return Err(unsupported_schema(version));
        }
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        connection.execute_batch(SCHEMA)?;
        while schema_version(&connection)? != SCHEMA_VERSION {
            // The write lock is taken before the version is read again, so a
            // step that a concurrent process has just completed is not
            // repeated. Each step commits on its own.
            let transaction = write_transaction(&mut connection)?;
            match schema_version(&transaction)? {
                1 => migrate_v1_to_v2(&transaction)?,
                2 => migrate_v2_to_v3(&transaction)?,
                3 => migrate_v3_to_v4(&transaction)?,
                4 => migrate_v4_to_v5(&transaction)?,
                SCHEMA_VERSION => {}
                version => return Err(unsupported_schema(version)),
            }
            transaction.commit()?;
        }
        Ok(())
    }

    /// Points agent homes and artifact paths that were recorded under another
    /// data directory at the same entries inside this database's directory.
    ///
    /// The database moves together with its data directory (the legacy
    /// `AgentDeck` directory move, or a state root the user relocated), but it
    /// records absolute paths. Only paths with the managed layout
    /// (`agent-homes/<agent>/<profile id>`, `checkpoints/<id>.json`,
    /// `handoffs/<id>`) are rewritten, and only when that entry exists in this
    /// data directory.
    fn relocate_managed_paths(&self) -> Result<usize> {
        let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        else {
            return Ok(0);
        };
        let data_dir =
            simplify_path(
                &std::path::absolute(parent).map_err(|source| ContextWakeError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?,
            );
        // (rows as `id, qualifier, path`, update, path components below the
        // data directory, the entry's path inside a data directory)
        let layouts: [(&str, &str, usize, ManagedEntry); 3] = [
            (
                "SELECT id, agent_id, agent_home FROM profiles WHERE agent_home IS NOT NULL",
                "UPDATE OR IGNORE profiles SET agent_home = ?1 WHERE id = ?2",
                3,
                agent_home_entry,
            ),
            (
                "SELECT id, '', storage_path FROM checkpoints WHERE storage_path IS NOT NULL",
                "UPDATE OR IGNORE checkpoints SET storage_path = ?1 WHERE id = ?2",
                2,
                checkpoint_entry,
            ),
            (
                "SELECT id, '', storage_path FROM handoffs WHERE storage_path IS NOT NULL",
                "UPDATE OR IGNORE handoffs SET storage_path = ?1 WHERE id = ?2",
                2,
                handoff_entry,
            ),
        ];
        let mut connection = self.connection()?;
        let mut relocations: Vec<(&str, String, PathBuf)> = Vec::new();
        for (select, update, depth, entry) in layouts {
            let mut statement = connection.prepare(select)?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (id, qualifier, stored) = row?;
                let expected = entry(&data_dir, &id, &qualifier);
                if is_relocated_entry(Path::new(&stored), &expected, depth) {
                    relocations.push((update, id, expected));
                }
            }
        }
        if relocations.is_empty() {
            return Ok(0);
        }
        let transaction = write_transaction(&mut connection)?;
        let mut relocated = 0;
        for (update, id, path) in &relocations {
            relocated += transaction.execute(update, params![path.to_string_lossy(), id])?;
        }
        transaction.commit()?;
        Ok(relocated)
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
        let mut statement = connection.prepare(select_profiles!(
            "ORDER BY COALESCE(last_used_at, created_at) DESC, name"
        ))?;
        let rows = statement.query_map([], profile_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Resolves a profile by ID (in any UUID spelling), then by name. IDs win,
    /// so a profile whose name looks like another profile's ID never shadows
    /// that profile.
    pub fn profile(&self, reference: &str) -> Result<Profile> {
        let connection = self.connection()?;
        resolve_profile(&connection, reference)
    }

    pub fn active_profile(&self) -> Result<Option<Profile>> {
        let connection = self.connection()?;
        let Some(id) = setting(&connection, "active_profile_id")? else {
            return Ok(None);
        };
        profile_by_id(&connection, &id)?
            .map(Some)
            .ok_or(ContextWakeError::ProfileNotFound(id))
    }

    pub fn set_active_profile(&self, id: Uuid) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(ContextWakeError::ProfileNotFound(id.to_string()));
        }
        let previous = setting(&transaction, "active_profile_id")?;
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
            return Err(ContextWakeError::ProfileNotFound(id.to_string()));
        }
        self.profile(&id.to_string())
    }

    pub fn remove_profile(&self, reference: &str) -> Result<Profile> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let profile = resolve_profile(&transaction, reference)?;
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

    /// Registers a workspace or refreshes the row for the same directory.
    ///
    /// Paths are stored in their conventional form, and an existing row whose
    /// path is spelled differently but names the same directory is updated
    /// instead of gaining a duplicate. Trust is sticky once granted. Trusted
    /// project preferences are refreshed so stored copies never go stale.
    pub fn upsert_workspace(&self, workspace: &Workspace) -> Result<()> {
        let path = stored_path(&workspace.path);
        let git_root = workspace.git_root.as_deref().map(stored_path);
        let git_root = git_root.as_ref().map(|value| value.to_string_lossy());
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        if let Some(existing) = workspace_id_for_path(&transaction, &path)? {
            transaction.execute(
                "UPDATE workspaces SET
                   path = ?1,
                   display_name = ?2,
                   trust_state = CASE WHEN trust_state = 'trusted' THEN 'trusted' ELSE ?3 END,
                   preferred_agent_id = ?4,
                   preferred_model_provider_id = ?5,
                   preferred_model = ?6,
                   git_root = ?7,
                   last_opened_at = ?8
                 WHERE id = ?9",
                params![
                    path.to_string_lossy(),
                    workspace.display_name,
                    workspace.trust_state.as_str(),
                    workspace.preferred_agent_id,
                    workspace.preferred_model_provider_id,
                    workspace.preferred_model,
                    git_root,
                    workspace.last_opened_at.to_rfc3339(),
                    existing,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO workspaces (
                   id, path, display_name, trust_state, preferred_agent_id,
                   preferred_model_provider_id, preferred_model, preferred_profile_id,
                   last_session_id, git_root, created_at, last_opened_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    workspace.id.to_string(),
                    path.to_string_lossy(),
                    workspace.display_name,
                    workspace.trust_state.as_str(),
                    workspace.preferred_agent_id,
                    workspace.preferred_model_provider_id,
                    workspace.preferred_model,
                    workspace
                        .preferred_profile_id
                        .map(|value| value.to_string()),
                    workspace.last_session_id.map(|value| value.to_string()),
                    git_root,
                    workspace.created_at.to_rfc3339(),
                    workspace.last_opened_at.to_rfc3339(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare(select_workspaces!("ORDER BY last_opened_at DESC"))?;
        let rows = statement.query_map([], workspace_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Resolves a workspace reference: an ID (in any UUID spelling, including
    /// the ID of a duplicate row that was merged into this one), a path in any
    /// spelling that names the same directory (relative, `.`, a trailing
    /// separator, and on Windows the `\\?\` form, `/` separators or another
    /// letter case), or a display name.
    ///
    /// A display name shared by several workspaces is refused with the
    /// candidates instead of resolving to an arbitrary one, so a destructive
    /// command never acts on a workspace the user did not mean.
    pub fn workspace(&self, reference: &str) -> Result<Workspace> {
        let connection = self.connection()?;
        resolve_workspace(&connection, reference)
    }

    /// Finds the workspace registered for `path`, comparing stored paths the
    /// way the filesystem does (see [`names_same_directory`]): a different
    /// directory whose path differs only in letter case is not a match.
    pub fn workspace_by_path(&self, path: &Path) -> Result<Option<Workspace>> {
        let connection = self.connection()?;
        match workspace_id_for_path(&connection, &stored_path(path))? {
            Some(id) => workspace_by_id(&connection, &id),
            None => Ok(None),
        }
    }

    pub fn active_workspace(&self) -> Result<Option<Workspace>> {
        let connection = self.connection()?;
        let Some(id) = setting(&connection, "active_workspace_id")? else {
            return Ok(None);
        };
        let workspace = if let Ok(uuid) = Uuid::parse_str(&id) {
            workspace_by_id_or_alias(&connection, uuid)?
        } else {
            None
        };
        workspace
            .map(Some)
            .ok_or(ContextWakeError::WorkspaceNotFound(id))
    }

    pub fn set_active_workspace(&self, id: Uuid) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(ContextWakeError::WorkspaceNotFound(id.to_string()));
        }
        transaction.execute(
            "INSERT INTO settings(key, value) VALUES ('active_workspace_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [id.to_string()],
        )?;
        transaction.execute(
            "UPDATE workspaces SET last_opened_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Resolves `reference` like [`Self::workspace`] and reports what
    /// [`Self::remove_workspace_with_report`] would delete, without changing
    /// anything.
    pub fn workspace_removal_preview(&self, reference: &str) -> Result<WorkspaceRemoval> {
        let connection = self.connection()?;
        workspace_removal(&connection, reference)
    }

    /// Deletes a workspace's metadata together with its session records and
    /// checkpoint index entries, and reports exactly what was deleted. An
    /// ambiguous reference is refused before anything is deleted.
    pub fn remove_workspace_with_report(&self, reference: &str) -> Result<WorkspaceRemoval> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let removal = workspace_removal(&transaction, reference)?;
        let id = removal.workspace.id.to_string();
        transaction.execute(
            "DELETE FROM settings WHERE key = 'active_workspace_id' AND value = ?1",
            [&id],
        )?;
        // Explicit deletes keep the report exact for databases whose migrated
        // tables lack the ON DELETE CASCADE constraints.
        transaction.execute("DELETE FROM sessions WHERE workspace_id = ?1", [&id])?;
        transaction.execute("DELETE FROM checkpoints WHERE workspace_id = ?1", [&id])?;
        transaction.execute(
            "DELETE FROM workspace_aliases WHERE workspace_id = ?1",
            [&id],
        )?;
        transaction.execute("DELETE FROM workspaces WHERE id = ?1", [&id])?;
        transaction.commit()?;
        Ok(removal)
    }

    pub fn remove_workspace(&self, reference: &str) -> Result<Workspace> {
        Ok(self.remove_workspace_with_report(reference)?.workspace)
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
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
    ///
    /// A refresh keeps the user's archive decision and stores the resume
    /// capability the caller computed from the adapter's current declaration,
    /// never a hard-coded one: a session synced while native resume was only
    /// partial stays non-native on every later sync, and becomes native once
    /// the adapter declares native resume verified.
    pub fn upsert_provider_session(&self, session: &Session) -> Result<bool> {
        let provider_session_id = session.provider_session_id.as_deref().ok_or_else(|| {
            ContextWakeError::InvalidData(
                "provider session synchronization requires a provider session ID".into(),
            )
        })?;
        let profile_id = session.profile_id.ok_or_else(|| {
            ContextWakeError::InvalidData(
                "provider session synchronization requires a profile".into(),
            )
        })?;
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let existing: Option<(String, bool)> = transaction
            .query_row(
                "SELECT id, archived FROM sessions
                 WHERE agent_id = ?1 AND profile_id = ?2 AND provider_session_id = ?3
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![
                    session.agent_id,
                    profile_id.to_string(),
                    provider_session_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let last_seen_at = session.last_seen_at.to_rfc3339();
        let (inserted, persisted_id, archived) = if let Some((id, archived)) = existing {
            transaction.execute(
                "UPDATE sessions
                 SET title = ?1, workspace_id = ?2, model_provider_id = ?3, model = ?4,
                     started_at = ?5, last_seen_at = ?6, resume_capability = ?7
                 WHERE id = ?8",
                params![
                    session.title,
                    session.workspace_id.to_string(),
                    session.model_provider_id,
                    session.model,
                    session.started_at.to_rfc3339(),
                    last_seen_at,
                    session.resume_capability.as_str(),
                    id,
                ],
            )?;
            (false, id, archived)
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
                    last_seen_at,
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
            (true, session.id.to_string(), session.archived)
        };
        if !archived {
            advance_workspace_activity(
                &transaction,
                &session.workspace_id.to_string(),
                &persisted_id,
                &last_seen_at,
            )?;
        }
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn record_native_resume(&self, reference: &str) -> Result<Session> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let session = resolve_session(&transaction, reference)?;
        let id = session.id.to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions
             SET last_seen_at = ?1, archived = 0, resume_capability = 'native',
                 continuity = 'native_resume'
             WHERE id = ?2",
            params![now, id],
        )?;
        transaction.execute(
            "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
            params![id, now, session.workspace_id.to_string()],
        )?;
        let updated = session_by_id(&transaction, &id)?
            .ok_or_else(|| ContextWakeError::SessionNotFound(reference.into()))?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn list_sessions(&self, include_archived: bool) -> Result<Vec<Session>> {
        let connection = self.connection()?;
        let sql = if include_archived {
            select_sessions!("ORDER BY last_seen_at DESC")
        } else {
            select_sessions!("WHERE archived = 0 ORDER BY last_seen_at DESC")
        };
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([], session_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Resolves a session by local ID, then by provider session ID. A provider
    /// session ID recorded for several local sessions (for example under two
    /// profiles) is refused with the candidates.
    pub fn session(&self, reference: &str) -> Result<Session> {
        let connection = self.connection()?;
        resolve_session(&connection, reference)
    }

    /// Records how the latest launch into an existing session row continued
    /// (for example `resumed_with_handoff`, or `unknown` when delivery could
    /// not be confirmed). The archive state and resume capability are kept.
    pub fn update_session_continuity(
        &self,
        id: Uuid,
        continuity: crate::model::ContinuityKind,
    ) -> Result<Session> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let id = id.to_string();
        let changed = transaction.execute(
            "UPDATE sessions SET continuity = ?1 WHERE id = ?2",
            params![continuity.as_str(), id],
        )?;
        if changed == 0 {
            return Err(ContextWakeError::SessionNotFound(id));
        }
        let updated = session_by_id(&transaction, &id)?
            .ok_or_else(|| ContextWakeError::SessionNotFound(id))?;
        transaction.commit()?;
        Ok(updated)
    }

    pub fn archive_session(&self, reference: &str, archived: bool) -> Result<Session> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let session = resolve_session(&transaction, reference)?;
        let id = session.id.to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE sessions SET archived = ?1, last_seen_at = ?2 WHERE id = ?3",
            params![archived, now, id],
        )?;
        if archived {
            transaction.execute(
                "UPDATE workspaces SET last_session_id = NULL WHERE last_session_id = ?1",
                [&id],
            )?;
        } else {
            transaction.execute(
                "UPDATE workspaces SET last_session_id = ?1, last_opened_at = ?2 WHERE id = ?3",
                params![id, now, session.workspace_id.to_string()],
            )?;
        }
        let updated = session_by_id(&transaction, &id)?
            .ok_or_else(|| ContextWakeError::SessionNotFound(reference.into()))?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Indexes a checkpoint file. The index row and its usage event commit
    /// together, so a failure leaves no index row behind for a file that the
    /// caller then removes.
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
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        transaction.execute(
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
        transaction.execute(
            "INSERT INTO usage_events(source, metric, value, profile_id, observed_at)
             VALUES ('local', 'checkpoint_count', 1, ?1, ?2)",
            params![
                profile_id.map(|value| value.to_string()),
                created_at.to_rfc3339()
            ],
        )?;
        transaction.commit()?;
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
            .ok_or_else(|| ContextWakeError::CheckpointNotFound(reference.into()))
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

    /// Every checkpoint index row, newest first. Reading the index never opens
    /// a checkpoint file, so one missing or corrupt file cannot hide the other
    /// entries.
    pub fn list_checkpoint_index(&self) -> Result<Vec<CheckpointIndexEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, objective, storage_path, created_at
             FROM checkpoints ORDER BY created_at DESC, id",
        )?;
        let rows = statement.query_map([], checkpoint_index_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The checkpoint index rows of one workspace, newest first, served by
    /// `checkpoints_workspace_idx`.
    pub fn workspace_checkpoint_index(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<CheckpointIndexEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, objective, storage_path, created_at
             FROM checkpoints WHERE workspace_id = ?1 ORDER BY created_at DESC, id",
        )?;
        let rows = statement.query_map([workspace_id.to_string()], checkpoint_index_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Index rows whose checkpoint file no longer exists (for example deleted
    /// or quarantined outside `ContextWake`). [`Self::delete_checkpoint_index`]
    /// removes such a row without touching the filesystem.
    pub fn missing_checkpoint_files(&self) -> Result<Vec<CheckpointIndexEntry>> {
        Ok(self
            .list_checkpoint_index()?
            .into_iter()
            .filter(|entry| {
                matches!(
                    std::fs::symlink_metadata(&entry.storage_path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            })
            .collect())
    }

    /// Deletes one checkpoint index row and returns the file path it recorded.
    /// The row is removed whether or not that file still exists.
    pub fn delete_checkpoint_index(&self, reference: &str) -> Result<PathBuf> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        let path = transaction
            .query_row(
                "SELECT storage_path FROM checkpoints WHERE id = ?1",
                [reference],
                |row| row.get::<_, String>(0).map(PathBuf::from),
            )
            .optional()?
            .ok_or_else(|| ContextWakeError::CheckpointNotFound(reference.into()))?;
        transaction.execute("DELETE FROM checkpoints WHERE id = ?1", [reference])?;
        transaction.commit()?;
        Ok(path)
    }

    /// Indexes a handoff directory; the row and its usage event commit
    /// together.
    pub fn insert_handoff(&self, record: &HandoffRecord) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = write_transaction(&mut connection)?;
        transaction.execute(
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
        transaction.execute(
            "INSERT INTO usage_events(source, metric, value, observed_at)
             VALUES ('local', 'handoff_count', 1, ?1)",
            [record.created_at.to_rfc3339()],
        )?;
        transaction.commit()?;
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
            .ok_or_else(|| ContextWakeError::HandoffNotFound(reference.into()))
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

    /// Removes a handoff index row, for rolling back a handoff whose creating
    /// operation (a profile switch) failed afterwards.
    pub fn delete_handoff_index(&self, id: Uuid) -> Result<()> {
        let connection = self.connection()?;
        connection.execute("DELETE FROM handoffs WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    /// Checkpoint index entries of one workspace, newest first, served by
    /// `checkpoints_workspace_idx` so callers can read a single file.
    pub fn list_checkpoint_paths_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<(Uuid, String, PathBuf, DateTime<Utc>)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, objective, storage_path, created_at
             FROM checkpoints WHERE workspace_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([workspace_id.to_string()], |row| {
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

fn setting(connection: &Connection, key: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn profile_by_id(connection: &Connection, id: &str) -> Result<Option<Profile>> {
    Ok(connection
        .query_row(select_profiles!("WHERE id = ?1"), [id], profile_from_row)
        .optional()?)
}

fn resolve_profile(connection: &Connection, reference: &str) -> Result<Profile> {
    if let Ok(id) = Uuid::parse_str(reference)
        && let Some(profile) = profile_by_id(connection, &id.to_string())?
    {
        return Ok(profile);
    }
    // Names are unique without regard to ASCII case.
    connection
        .query_row(
            select_profiles!("WHERE name = ?1 COLLATE NOCASE"),
            [reference],
            profile_from_row,
        )
        .optional()?
        .ok_or_else(|| ContextWakeError::ProfileNotFound(reference.into()))
}

fn workspace_by_id(connection: &Connection, id: &str) -> Result<Option<Workspace>> {
    Ok(connection
        .query_row(
            select_workspaces!("WHERE id = ?1"),
            [id],
            workspace_from_row,
        )
        .optional()?)
}

/// Looks a workspace up by ID, falling back to the IDs of duplicate rows that
/// were merged into it (checkpoint files keep recording the old ID).
fn workspace_by_id_or_alias(connection: &Connection, id: Uuid) -> Result<Option<Workspace>> {
    let id = id.to_string();
    if let Some(workspace) = workspace_by_id(connection, &id)? {
        return Ok(Some(workspace));
    }
    Ok(connection
        .query_row(
            select_workspaces!(
                "WHERE id = (SELECT workspace_id FROM workspace_aliases WHERE alias_id = ?1)"
            ),
            [&id],
            workspace_from_row,
        )
        .optional()?)
}

fn resolve_workspace(connection: &Connection, reference: &str) -> Result<Workspace> {
    if let Ok(id) = Uuid::parse_str(reference)
        && let Some(workspace) = workspace_by_id_or_alias(connection, id)?
    {
        return Ok(workspace);
    }
    if let Some(workspace) = connection
        .query_row(
            select_workspaces!("WHERE path = ?1"),
            [reference],
            workspace_from_row,
        )
        .optional()?
    {
        return Ok(workspace);
    }
    let (kind, candidates) = if looks_like_path(reference) {
        (
            "path",
            workspaces_at_path(connection, Path::new(reference))?,
        )
    } else {
        let mut statement = connection.prepare(select_workspaces!(
            "WHERE display_name = ?1 COLLATE NOCASE ORDER BY created_at, id"
        ))?;
        let rows = statement.query_map([reference], workspace_from_row)?;
        ("name", rows.collect::<std::result::Result<Vec<_>, _>>()?)
    };
    if candidates.len() > 1 {
        let listed = candidates
            .iter()
            .fold(String::new(), |mut listed, workspace| {
                let _ = write!(listed, "\n  {}  {}", workspace.id, workspace.path.display());
                listed
            });
        return Err(ContextWakeError::AmbiguousReference(format!(
            "workspace {kind} '{reference}' matches {} registered workspaces; nothing was changed. Retry with one of these workspace IDs or paths:{listed}",
            candidates.len()
        )));
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| ContextWakeError::WorkspaceNotFound(reference.into()))
}

/// Display names never contain path separators, so a reference that does (or
/// is `.`, `..`, rooted or drive-qualified) is resolved as a path.
fn looks_like_path(reference: &str) -> bool {
    let path = Path::new(reference);
    reference == "."
        || reference == ".."
        || reference.chars().any(std::path::is_separator)
        || path.has_root()
        || matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        )
}

/// True when the stored workspace path `stored` names the directory
/// `candidate` names.
///
/// Spellings that differ only in the verbatim prefix, separators or a trailing
/// separator always match. Other equivalent spellings (on Windows, letter
/// case) are decided by the filesystem: when both paths exist, their canonical
/// forms must be identical, case included. On a case-insensitive volume both
/// spellings canonicalize to the on-disk name and match; in a case-sensitive
/// directory `Repo` and `repo` are different directories and do not, so one
/// never inherits the other's row or trust. Two spellings of the same
/// directory either both exist or both do not, so when exactly one exists they
/// name different directories. Only when neither exists (a deleted or moved
/// checkout) is the lexical comparison all there is.
fn names_same_directory(stored: &Path, candidate: &Path) -> bool {
    if path_merge_key(stored) == path_merge_key(candidate) {
        return true;
    }
    if !paths_equivalent(stored, candidate) {
        return false;
    }
    match (
        canonicalize_simplified(stored),
        canonicalize_simplified(candidate),
    ) {
        (Ok(stored), Ok(candidate)) => path_merge_key(&stored) == path_merge_key(&candidate),
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

/// Workspaces registered at the directory `reference` names. An existing path
/// is compared in canonical form, which resolves `.` and `..`, symlinks,
/// letter case and the verbatim prefix. A path that no longer exists (a
/// deleted or moved checkout) is compared lexically. See
/// [`names_same_directory`].
fn workspaces_at_path(connection: &Connection, reference: &Path) -> Result<Vec<Workspace>> {
    let workspaces = {
        let mut statement = connection.prepare(select_workspaces!("ORDER BY created_at, id"))?;
        let rows = statement.query_map([], workspace_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let spellings = canonicalize_simplified(reference).ok().into_iter().chain(
        std::path::absolute(reference)
            .ok()
            .map(|path| simplify_path(&path)),
    );
    for spelling in spellings {
        let matches = workspaces
            .iter()
            .filter(|workspace| names_same_directory(&workspace.path, &spelling))
            .cloned()
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return Ok(matches);
        }
    }
    Ok(Vec::new())
}

/// The ID of the row whose stored path names `path`: the exact stored text
/// first, then another spelling of the same directory (oldest registration
/// first; see [`names_same_directory`]).
fn workspace_id_for_path(connection: &Connection, path: &Path) -> Result<Option<String>> {
    let exact: Option<String> = connection
        .query_row(
            "SELECT id FROM workspaces WHERE path = ?1",
            [path.to_string_lossy()],
            |row| row.get(0),
        )
        .optional()?;
    if exact.is_some() {
        return Ok(exact);
    }
    let mut statement =
        connection.prepare("SELECT id, path FROM workspaces ORDER BY created_at, id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, stored) = row?;
        if names_same_directory(Path::new(&stored), path) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn workspace_removal(connection: &Connection, reference: &str) -> Result<WorkspaceRemoval> {
    let workspace = resolve_workspace(connection, reference)?;
    let id = workspace.id.to_string();
    let session_records: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sessions WHERE workspace_id = ?1",
        [&id],
        |row| row.get(0),
    )?;
    let checkpoint_files = {
        let mut statement = connection.prepare(
            "SELECT storage_path FROM checkpoints WHERE workspace_id = ?1
             ORDER BY created_at DESC, id",
        )?;
        let rows = statement.query_map([&id], |row| row.get::<_, String>(0).map(PathBuf::from))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    Ok(WorkspaceRemoval {
        workspace,
        session_records: usize::try_from(session_records).unwrap_or_default(),
        checkpoint_records: checkpoint_files.len(),
        checkpoint_files,
    })
}

fn session_by_id(connection: &Connection, id: &str) -> Result<Option<Session>> {
    Ok(connection
        .query_row(select_sessions!("WHERE id = ?1"), [id], session_from_row)
        .optional()?)
}

fn resolve_session(connection: &Connection, reference: &str) -> Result<Session> {
    if let Ok(id) = Uuid::parse_str(reference)
        && let Some(session) = session_by_id(connection, &id.to_string())?
    {
        return Ok(session);
    }
    let mut statement = connection.prepare(select_sessions!(
        "WHERE provider_session_id = ?1 ORDER BY last_seen_at DESC, id"
    ))?;
    let candidates = statement
        .query_map([reference], session_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if candidates.len() > 1 {
        let listed = candidates
            .iter()
            .fold(String::new(), |mut listed, session| {
                let _ = write!(
                    listed,
                    "\n  {}  {}  {}",
                    session.id,
                    session.agent_id,
                    session.title.as_deref().unwrap_or("untitled")
                );
                listed
            });
        return Err(ContextWakeError::AmbiguousReference(format!(
            "provider session ID '{reference}' is recorded for {} local sessions; nothing was changed. Retry with one of these local session IDs:{listed}",
            candidates.len()
        )));
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| ContextWakeError::SessionNotFound(reference.into()))
}

/// Makes `session_id` the workspace's last session unless a newer, unarchived
/// session already is, and never moves `last_opened_at` backwards. Provider
/// discovery reports sessions in any order and with past timestamps.
fn advance_workspace_activity(
    connection: &Connection,
    workspace_id: &str,
    session_id: &str,
    last_seen_at: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE workspaces SET last_session_id = ?2
         WHERE id = ?1
           AND (last_session_id IS NULL
                OR last_session_id = ?2
                OR NOT EXISTS (
                  SELECT 1 FROM sessions AS latest
                  WHERE latest.id = workspaces.last_session_id
                    AND latest.archived = 0
                    AND latest.last_seen_at > ?3
                ))",
        params![workspace_id, session_id, last_seen_at],
    )?;
    connection.execute(
        "UPDATE workspaces SET last_opened_at = ?2 WHERE id = ?1 AND last_opened_at < ?2",
        params![workspace_id, last_seen_at],
    )?;
    Ok(())
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
        home_mode: crate::model::ProfileHomeMode::Isolated,
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

fn checkpoint_index_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointIndexEntry> {
    Ok(CheckpointIndexEntry {
        id: parse_uuid(row.get(0)?)?,
        workspace_id: parse_uuid(row.get(1)?)?,
        objective: row.get(2)?,
        storage_path: PathBuf::from(row.get::<_, String>(3)?),
        created_at: parse_time(row.get(4)?)?,
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

    fn test_profile(directory: &Path, name: &str, agent_id: &str) -> Profile {
        let now = Utc::now();
        Profile {
            id: Uuid::new_v4(),
            name: name.into(),
            display_name: name.into(),
            agent_id: agent_id.into(),
            model_provider_id: None,
            model_preference: None,
            description: None,
            agent_home: directory.join(format!("{name}-home")),
            home_mode: crate::model::ProfileHomeMode::Isolated,
            account_fingerprint: None,
            auth_state: AuthState::Unknown,
            created_at: now,
            updated_at: now,
            last_used_at: None,
        }
    }

    fn test_workspace(path: PathBuf, display_name: &str) -> Workspace {
        let now = Utc::now();
        Workspace {
            id: Uuid::new_v4(),
            path,
            display_name: display_name.into(),
            trust_state: TrustState::Untrusted,
            preferred_agent_id: None,
            preferred_model_provider_id: None,
            preferred_model: None,
            preferred_profile_id: None,
            last_session_id: None,
            git_root: None,
            created_at: now,
            last_opened_at: now,
        }
    }

    fn discovered_session(
        workspace: &Workspace,
        profile: &Profile,
        provider_session_id: &str,
        resume_capability: ResumeCapability,
    ) -> Session {
        let now = Utc::now();
        Session {
            id: Uuid::new_v4(),
            provider_session_id: Some(provider_session_id.into()),
            title: None,
            workspace_id: workspace.id,
            profile_id: Some(profile.id),
            agent_id: profile.agent_id.clone(),
            model_provider_id: None,
            model: None,
            started_at: now,
            last_seen_at: now,
            resume_capability,
            archived: false,
            continuity: ContinuityKind::Unknown,
        }
    }

    #[cfg(windows)]
    fn rfc3339(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

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
            home_mode: crate::model::ProfileHomeMode::Isolated,
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
            home_mode: crate::model::ProfileHomeMode::Isolated,
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
            home_mode: crate::model::ProfileHomeMode::Isolated,
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
            home_mode: crate::model::ProfileHomeMode::Isolated,
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
    fn provider_session_refresh_follows_the_adapter_capability_and_keeps_archive() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let profile = test_profile(directory.path(), "grok", "grok");
        store.insert_profile(&profile).expect("profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");

        // An adapter whose native resume is only partial is synced as unknown.
        let discovered =
            discovered_session(&workspace, &profile, "grok-1", ResumeCapability::Unknown);
        assert!(
            store
                .upsert_provider_session(&discovered)
                .expect("first sync")
        );
        let resync = Session {
            id: Uuid::new_v4(),
            last_seen_at: discovered.last_seen_at + chrono::Duration::minutes(1),
            ..discovered.clone()
        };
        assert!(!store.upsert_provider_session(&resync).expect("second sync"));
        assert_eq!(
            store.session("grok-1").expect("session").resume_capability,
            ResumeCapability::Unknown,
            "re-running sync must not unlock native resume"
        );

        // Once the adapter declares native resume verified, sync computes
        // native and the refreshed row follows instead of staying frozen.
        let upgraded = Session {
            id: Uuid::new_v4(),
            resume_capability: ResumeCapability::Native,
            ..resync.clone()
        };
        store
            .upsert_provider_session(&upgraded)
            .expect("third sync");
        assert_eq!(
            store.session("grok-1").unwrap().resume_capability,
            ResumeCapability::Native,
            "a verified adapter declaration applies to rows synced earlier"
        );

        // An archived session stays archived and is not made the workspace's
        // last session again.
        store.archive_session("grok-1", true).expect("archive");
        let after_archive = Session {
            id: Uuid::new_v4(),
            last_seen_at: discovered.last_seen_at + chrono::Duration::minutes(5),
            ..resync
        };
        assert!(
            !store
                .upsert_provider_session(&after_archive)
                .expect("sync after archive")
        );
        assert!(store.session("grok-1").unwrap().archived);
        assert!(store.list_sessions(false).unwrap().is_empty());
        assert_eq!(
            store
                .workspace(&workspace.id.to_string())
                .unwrap()
                .last_session_id,
            None
        );
    }

    #[test]
    fn provider_session_refresh_applies_a_lower_adapter_capability() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let profile = test_profile(directory.path(), "copilot", "github-copilot");
        store.insert_profile(&profile).expect("profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        let discovered =
            discovered_session(&workspace, &profile, "copilot-1", ResumeCapability::Native);
        store.upsert_provider_session(&discovered).expect("sync");
        let unsupported = Session {
            id: Uuid::new_v4(),
            resume_capability: ResumeCapability::HandoffOnly,
            ..discovered
        };
        store.upsert_provider_session(&unsupported).expect("resync");
        assert_eq!(
            store.session("copilot-1").unwrap().resume_capability,
            ResumeCapability::HandoffOnly
        );
    }

    #[test]
    fn provider_session_sync_keeps_the_most_recent_session_as_last() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let profile = test_profile(directory.path(), "codex", "codex");
        store.insert_profile(&profile).expect("profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        let mut newer = discovered_session(&workspace, &profile, "newer", ResumeCapability::Native);
        newer.last_seen_at = workspace.last_opened_at + chrono::Duration::minutes(10);
        let mut older = discovered_session(&workspace, &profile, "older", ResumeCapability::Native);
        older.started_at = workspace.last_opened_at - chrono::Duration::hours(3);
        older.last_seen_at = workspace.last_opened_at - chrono::Duration::hours(1);

        // Agents list their sessions newest first.
        store.upsert_provider_session(&newer).expect("newer");
        store.upsert_provider_session(&older).expect("older");
        let refreshed = store.workspace(&workspace.id.to_string()).unwrap();
        assert_eq!(refreshed.last_session_id, Some(newer.id));
        assert_eq!(refreshed.last_opened_at, newer.last_seen_at);
    }

    #[test]
    fn ambiguous_workspace_names_are_refused_before_removal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let clients = directory.path().join("clients");
        let first = test_workspace(clients.join("a").join("api"), "api");
        let second = Workspace {
            created_at: first.created_at + chrono::Duration::seconds(1),
            ..test_workspace(clients.join("b").join("api"), "api")
        };
        store.upsert_workspace(&first).expect("first");
        store.upsert_workspace(&second).expect("second");

        for reference in ["api", "API"] {
            let error = store.workspace(reference).expect_err("ambiguous name");
            let ContextWakeError::AmbiguousReference(message) = &error else {
                panic!("expected an ambiguity error, got {error}");
            };
            for workspace in [&first, &second] {
                assert!(message.contains(&workspace.id.to_string()), "{message}");
                assert!(
                    message.contains(&workspace.path.display().to_string()),
                    "{message}"
                );
            }
        }
        assert!(matches!(
            store.remove_workspace("api"),
            Err(ContextWakeError::AmbiguousReference(_))
        ));
        assert_eq!(
            store.list_workspaces().unwrap().len(),
            2,
            "nothing is removed"
        );

        // IDs and paths still identify each workspace exactly.
        assert_eq!(
            store.workspace(&second.id.to_string()).unwrap().id,
            second.id
        );
        assert_eq!(
            store
                .workspace(&second.id.to_string().to_uppercase())
                .unwrap()
                .id,
            second.id
        );
        assert_eq!(
            store
                .remove_workspace(&second.path.to_string_lossy())
                .unwrap()
                .id,
            second.id
        );
        assert_eq!(store.workspace("api").unwrap().id, first.id);
    }

    #[test]
    fn workspace_removal_reports_its_records() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let profile = test_profile(directory.path(), "work", "codex");
        store.insert_profile(&profile).expect("profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        let other = test_workspace(directory.path().join("other"), "other");
        store.upsert_workspace(&workspace).expect("workspace");
        store.upsert_workspace(&other).expect("other");
        store.set_active_workspace(workspace.id).expect("activate");
        store
            .insert_session(&discovered_session(
                &workspace,
                &profile,
                "s1",
                ResumeCapability::Native,
            ))
            .expect("session");
        store
            .insert_session(&discovered_session(
                &other,
                &profile,
                "s2",
                ResumeCapability::Native,
            ))
            .expect("other session");
        let checkpoint_file = directory.path().join("checkpoint.json");
        store
            .insert_checkpoint_index(
                Uuid::new_v4(),
                workspace.id,
                None,
                None,
                None,
                None,
                None,
                "Objective",
                &checkpoint_file,
                RedactionStatus::Clean,
                Utc::now(),
            )
            .expect("checkpoint");

        let preview = store
            .workspace_removal_preview(&workspace.id.to_string())
            .expect("preview");
        assert_eq!(preview.workspace.id, workspace.id);
        assert_eq!(preview.session_records, 1);
        assert_eq!(preview.checkpoint_records, 1);
        assert_eq!(preview.checkpoint_files, vec![checkpoint_file.clone()]);
        assert_eq!(store.list_workspaces().unwrap().len(), 2);

        let removal = store
            .remove_workspace_with_report(&workspace.id.to_string())
            .expect("remove");
        assert_eq!(removal, preview);
        assert_eq!(store.active_workspace().unwrap(), None);
        let remaining = store.list_sessions(true).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].workspace_id, other.id);
        assert!(store.list_checkpoint_index().unwrap().is_empty());
    }

    #[test]
    fn workspace_paths_resolve_in_equivalent_spellings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let root = canonicalize_simplified(directory.path()).expect("canonical root");
        let repository = root.join("Repo Dir");
        std::fs::create_dir_all(repository.join("child")).expect("repository");
        let workspace = test_workspace(repository.clone(), "Repo Dir");
        store.upsert_workspace(&workspace).expect("workspace");

        let text = repository.to_string_lossy().into_owned();
        let mut spellings = vec![
            format!("{text}{}", std::path::MAIN_SEPARATOR),
            repository
                .join("child")
                .join("..")
                .to_string_lossy()
                .into_owned(),
        ];
        if cfg!(windows) {
            spellings.push(format!(r"\\?\{text}"));
            spellings.push(text.to_uppercase());
            spellings.push(text.replace('\\', "/"));
        }
        for spelling in &spellings {
            assert_eq!(
                store.workspace(spelling).expect(spelling).id,
                workspace.id,
                "{spelling}"
            );
        }

        // A checkout that no longer exists still resolves by its path.
        std::fs::remove_dir_all(&repository).expect("remove repository");
        assert_eq!(
            store
                .workspace(&spellings[0])
                .expect("missing directory")
                .id,
            workspace.id
        );
        assert!(matches!(
            store.workspace(&root.join("elsewhere").to_string_lossy()),
            Err(ContextWakeError::WorkspaceNotFound(_))
        ));
    }

    #[test]
    fn workspace_refresh_keeps_trust_and_updates_project_preferences() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let untrusted = test_workspace(directory.path().join("repo"), "repo");
        store
            .upsert_workspace(&untrusted)
            .expect("first registration");
        let trusted = Workspace {
            id: Uuid::new_v4(),
            trust_state: TrustState::Trusted,
            preferred_agent_id: Some("codex".into()),
            preferred_model: Some("gpt-5.3-codex".into()),
            ..untrusted.clone()
        };
        store
            .upsert_workspace(&trusted)
            .expect("trusted registration");
        let stored = store.workspace(&untrusted.id.to_string()).unwrap();
        assert_eq!(stored.trust_state, TrustState::Trusted);
        assert_eq!(stored.preferred_agent_id.as_deref(), Some("codex"));
        assert_eq!(stored.preferred_model.as_deref(), Some("gpt-5.3-codex"));

        // Trust stays sticky when an untrusted caller registers it again.
        store
            .upsert_workspace(&untrusted)
            .expect("untrusted refresh");
        assert_eq!(
            store
                .workspace(&untrusted.id.to_string())
                .unwrap()
                .trust_state,
            TrustState::Trusted
        );
        assert_eq!(store.list_workspaces().unwrap().len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn equivalent_workspace_paths_share_one_conventional_row() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let repository = canonicalize_simplified(directory.path())
            .expect("canonical")
            .join("repo");
        std::fs::create_dir_all(&repository).expect("repository");
        let verbatim = PathBuf::from(format!(r"\\?\{}", repository.display()));
        let first = Workspace {
            git_root: Some(verbatim.clone()),
            ..test_workspace(verbatim.clone(), "repo")
        };
        store
            .upsert_workspace(&first)
            .expect("verbatim registration");
        let stored = store.list_workspaces().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].path, repository);
        assert_eq!(stored[0].git_root.as_deref(), Some(repository.as_path()));

        let upper_case = PathBuf::from(repository.to_string_lossy().to_uppercase());
        store
            .upsert_workspace(&test_workspace(upper_case.clone(), "repo"))
            .expect("case-variant registration");
        assert_eq!(
            store.list_workspaces().unwrap().len(),
            1,
            "a case-insensitive directory spelled in another case is the same row"
        );
        assert_eq!(
            store
                .workspace_by_path(&upper_case)
                .expect("lookup")
                .expect("registered")
                .id,
            first.id
        );

        let forward_slashes = PathBuf::from(repository.to_string_lossy().replace('\\', "/"));
        store
            .upsert_workspace(&test_workspace(forward_slashes, "repo"))
            .expect("equivalent registration");
        let stored = store.list_workspaces().unwrap();
        assert_eq!(stored.len(), 1, "no duplicate row for the same directory");
        assert_eq!(stored[0].id, first.id);
        assert_eq!(
            stored[0].path.as_os_str(),
            repository.as_os_str(),
            "the stored spelling stays conventional"
        );
        assert_eq!(
            store
                .workspace_by_path(&verbatim)
                .expect("lookup")
                .expect("registered")
                .id,
            first.id
        );
    }

    /// Creates `path` as an empty directory with per-directory case
    /// sensitivity enabled. Returns `false` when this Windows installation does
    /// not support it.
    #[cfg(windows)]
    fn case_sensitive_directory(path: &Path) -> bool {
        std::fs::create_dir_all(path).expect("directory");
        let system_root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        std::process::Command::new(system_root.join("System32").join("fsutil.exe"))
            .args(["file", "setCaseSensitiveInfo"])
            .arg(path)
            .arg("enable")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    #[test]
    fn case_variant_directories_in_a_case_sensitive_folder_stay_separate() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let folder = canonicalize_simplified(directory.path())
            .expect("canonical")
            .join("case-sensitive");
        if !case_sensitive_directory(&folder) {
            eprintln!("skipping: per-directory case sensitivity is not supported here");
            return;
        }
        let upper = folder.join("Repo");
        let lower = folder.join("repo");
        std::fs::create_dir(&upper).expect("Repo");
        if std::fs::create_dir(&lower).is_err()
            || std::fs::read_dir(&folder).expect("read folder").count() != 2
        {
            eprintln!("skipping: the folder did not become case-sensitive");
            return;
        }
        let trusted = Workspace {
            trust_state: TrustState::Trusted,
            ..test_workspace(upper.clone(), "Repo")
        };
        store.upsert_workspace(&trusted).expect("trusted workspace");
        let lower_text = lower.to_string_lossy().into_owned();

        // The other directory is not the trusted workspace.
        assert!(store.workspace_by_path(&lower).expect("lookup").is_none());
        assert!(matches!(
            store.workspace(&lower_text),
            Err(ContextWakeError::WorkspaceNotFound(_))
        ));
        assert!(matches!(
            store.remove_workspace(&lower_text),
            Err(ContextWakeError::WorkspaceNotFound(_))
        ));
        assert_eq!(store.list_workspaces().unwrap().len(), 1);

        // Registering it creates a separate untrusted row and leaves the
        // trusted row's path alone.
        let lookalike = test_workspace(lower.clone(), "lookalike");
        store
            .upsert_workspace(&lookalike)
            .expect("second workspace");
        assert_eq!(store.list_workspaces().unwrap().len(), 2);
        let original = store.workspace(&trusted.id.to_string()).unwrap();
        assert_eq!(original.path, upper);
        assert_eq!(original.trust_state, TrustState::Trusted);
        let registered = store.workspace(&lower_text).expect("lookalike");
        assert_eq!(registered.id, lookalike.id);
        assert_eq!(registered.trust_state, TrustState::Untrusted);
        assert_eq!(
            store.workspace(&upper.to_string_lossy()).unwrap().id,
            trusted.id
        );

        // Removing by the other path removes only that row.
        assert_eq!(
            store.remove_workspace(&lower_text).expect("remove").id,
            lookalike.id
        );
        let remaining = store.list_workspaces().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, trusted.id);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_migrate_to_conventional_form_and_duplicates_merge() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let repository = canonicalize_simplified(directory.path())
            .expect("canonical")
            .join("repo");
        std::fs::create_dir_all(&repository).expect("repository");
        let conventional = repository.to_string_lossy().into_owned();
        let verbatim = format!(r"\\?\{conventional}");
        let store = Store::open(&path).expect("store");
        let profile = test_profile(directory.path(), "work", "codex");
        store.insert_profile(&profile).expect("profile");
        drop(store);

        // Rows as a v4 build recorded them: the canonical (verbatim) row, and
        // a conventional duplicate that the user trusted.
        let older = Uuid::new_v4();
        let newer = Uuid::new_v4();
        let checkpoint = Uuid::new_v4();
        let handoff = Uuid::new_v4();
        let checkpoint_path = directory.path().join("checkpoint.json");
        let handoff_path = directory.path().join("handoff");
        let connection = Connection::open(&path).expect("connection");
        connection
            .execute("UPDATE schema_meta SET version = 4", [])
            .expect("v4");
        connection
            .execute(
                "INSERT INTO workspaces (id, path, display_name, trust_state, git_root, created_at, last_opened_at)
                 VALUES (?1, ?2, 'repo', 'untrusted', ?2, '2026-01-01T00:00:00+00:00', '2026-01-02T00:00:00+00:00')",
                params![older.to_string(), verbatim],
            )
            .expect("verbatim row");
        connection
            .execute(
                "INSERT INTO workspaces (id, path, display_name, trust_state, created_at, last_opened_at)
                 VALUES (?1, ?2, 'repo', 'trusted', '2026-02-01T00:00:00+00:00', '2026-03-01T00:00:00+00:00')",
                params![newer.to_string(), conventional],
            )
            .expect("conventional row");
        for (session, workspace) in [(Uuid::new_v4(), older), (Uuid::new_v4(), newer)] {
            connection
                .execute(
                    "INSERT INTO sessions (id, workspace_id, profile_id, agent_id, started_at, last_seen_at, resume_capability)
                     VALUES (?1, ?2, ?3, 'codex', '2026-02-01T00:00:00+00:00', '2026-02-01T00:00:00+00:00', 'unknown')",
                    params![session.to_string(), workspace.to_string(), profile.id.to_string()],
                )
                .expect("session");
        }
        connection
            .execute(
                "INSERT INTO checkpoints (id, workspace_id, objective, storage_path, redaction_status, created_at)
                 VALUES (?1, ?2, 'Continue', ?3, 'clean', '2026-02-01T00:00:00+00:00')",
                params![
                    checkpoint.to_string(),
                    newer.to_string(),
                    format!(r"\\?\{}", checkpoint_path.display())
                ],
            )
            .expect("checkpoint");
        connection
            .execute(
                "INSERT INTO handoffs (id, checkpoint_id, schema_version, storage_path, sha256, created_at)
                 VALUES (?1, ?2, '1.2.0', ?3, ?4, '2026-02-01T00:00:00+00:00')",
                params![
                    handoff.to_string(),
                    checkpoint.to_string(),
                    format!(r"\\?\{}", handoff_path.display()),
                    "0".repeat(64)
                ],
            )
            .expect("handoff");
        connection
            .execute(
                "UPDATE profiles SET agent_home = ?1 WHERE id = ?2",
                params![
                    format!(r"\\?\{}", profile.agent_home.display()),
                    profile.id.to_string()
                ],
            )
            .expect("verbatim agent home");
        connection
            .execute(
                "INSERT INTO settings(key, value) VALUES ('active_workspace_id', ?1)",
                [newer.to_string()],
            )
            .expect("active workspace");
        drop(connection);

        let store = Store::open(&path).expect("v5 migration");
        let workspaces = store.list_workspaces().unwrap();
        assert_eq!(workspaces.len(), 1, "{workspaces:#?}");
        let merged = &workspaces[0];
        assert_eq!(merged.id, older, "the oldest registration survives");
        assert_eq!(merged.path, repository);
        assert_eq!(merged.git_root.as_deref(), Some(repository.as_path()));
        assert_eq!(merged.trust_state, TrustState::Trusted);
        assert_eq!(merged.created_at, rfc3339("2026-01-01T00:00:00+00:00"));
        assert_eq!(merged.last_opened_at, rfc3339("2026-03-01T00:00:00+00:00"));
        assert!(
            store
                .list_sessions(true)
                .unwrap()
                .iter()
                .all(|session| session.workspace_id == older)
        );
        let index = store.list_checkpoint_index().unwrap();
        assert_eq!(index[0].workspace_id, older);
        assert_eq!(index[0].storage_path, checkpoint_path);
        assert_eq!(store.active_workspace().unwrap().unwrap().id, older);
        assert_eq!(
            store.workspace(&newer.to_string()).unwrap().id,
            older,
            "the merged ID stays resolvable for checkpoint files that record it"
        );
        assert_eq!(
            store.handoff(&handoff.to_string()).unwrap().storage_path,
            handoff_path
        );
        assert_eq!(
            store.profile("work").unwrap().agent_home,
            profile.agent_home
        );
        assert_eq!(
            schema_version(&store.connection().unwrap()).unwrap(),
            SCHEMA_VERSION
        );

        Store::open(&path).expect("idempotent reopen");
        assert_eq!(store.list_workspaces().unwrap().len(), 1);
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
        assert_eq!(workspace.preferred_profile_id, Some(profile_id));
        let session = store
            .session(&session_id.to_string())
            .expect("migrated session");
        assert_eq!(session.agent_id, "claude");
        assert_eq!(session.model.as_deref(), Some("sonnet"));
        assert_eq!(session.model_provider_id, None);
        assert_eq!(session.title, None);
        assert_eq!(session.profile_id, Some(profile_id));

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
        assert_eq!(version, SCHEMA_VERSION);

        // The migrated database accepts new records: the v2 NOT NULL
        // provider_* columns would otherwise reject them.
        let new_profile = test_profile(directory.path(), "personal", "codex");
        store
            .insert_profile(&new_profile)
            .expect("profiles can be added after a v2 migration");
        store
            .insert_session(&Session {
                id: Uuid::new_v4(),
                provider_session_id: None,
                title: None,
                workspace_id,
                profile_id: Some(new_profile.id),
                agent_id: "codex".into(),
                model_provider_id: None,
                model: None,
                started_at: Utc::now(),
                last_seen_at: Utc::now(),
                resume_capability: ResumeCapability::Unknown,
                archived: false,
                continuity: ContinuityKind::RestoredFromHandoff,
            })
            .expect("sessions can be added after a v2 migration");
    }

    #[test]
    fn databases_migrated_with_legacy_columns_accept_new_records() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let profile_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        let home = directory.path().join("legacy-home");
        // What earlier builds produced from a v2 database: the v4 columns next
        // to the NOT NULL provider_* columns, at version 4.
        let connection = Connection::open(&path).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (4);
                 CREATE TABLE profiles (
                   id TEXT PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                   display_name TEXT NOT NULL, provider_id TEXT NOT NULL,
                   description TEXT, provider_home TEXT NOT NULL UNIQUE,
                   account_fingerprint TEXT, auth_state TEXT NOT NULL,
                   created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_used_at TEXT,
                   agent_id TEXT NOT NULL DEFAULT 'codex', model_provider_id TEXT,
                   model_preference TEXT, agent_home TEXT
                 );
                 CREATE UNIQUE INDEX profiles_agent_home_idx ON profiles(agent_home);
                 CREATE TABLE sessions (
                   id TEXT PRIMARY KEY, provider_session_id TEXT, workspace_id TEXT NOT NULL,
                   profile_id TEXT, provider_id TEXT NOT NULL, model TEXT,
                   started_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                   resume_capability TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
                   continuity TEXT NOT NULL DEFAULT 'unknown',
                   agent_id TEXT NOT NULL DEFAULT 'codex', model_provider_id TEXT, title TEXT
                 );
                 CREATE TABLE usage_events (
                   id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
                   metric TEXT NOT NULL, value INTEGER NOT NULL,
                   profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
                   observed_at TEXT NOT NULL
                 );",
            )
            .expect("legacy v4 schema");
        connection
            .execute(
                "INSERT INTO profiles VALUES (?1, 'work', 'Work', 'claude', NULL, ?2, NULL, 'signed_in', ?3, ?3, NULL, 'claude', NULL, NULL, ?2)",
                params![profile_id.to_string(), home.to_string_lossy(), now],
            )
            .expect("profile");
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, 'claude-1', ?2, ?3, 'claude', NULL, ?4, ?4, 'native', 0, 'unknown', 'claude', NULL, 'Title')",
                params![
                    session_id.to_string(),
                    workspace_id.to_string(),
                    profile_id.to_string(),
                    now
                ],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO usage_events(source, metric, value, profile_id, observed_at) VALUES ('local', 'session_count', 1, ?1, ?2)",
                params![profile_id.to_string(), now],
            )
            .expect("usage event");
        drop(connection);

        let store = Store::open(&path).expect("repair migration");
        let connection = store.connection().expect("connection");
        for (table, column) in [
            ("profiles", "provider_id"),
            ("profiles", "provider_home"),
            ("sessions", "provider_id"),
        ] {
            assert!(
                !table_has_column(&connection, table, column).unwrap(),
                "{table}.{column} is gone"
            );
        }
        // Rows and the references to them survive the table rebuilds.
        let profile = store.profile(&profile_id.to_string()).expect("profile");
        assert_eq!(profile.agent_id, "claude");
        assert_eq!(profile.agent_home, home);
        assert_eq!(profile.auth_state, AuthState::SignedIn);
        let session = store.session("claude-1").expect("session");
        assert_eq!(session.id, session_id);
        assert_eq!(session.profile_id, Some(profile_id));
        assert_eq!(session.title.as_deref(), Some("Title"));
        let usage_profile: Option<String> = connection
            .query_row("SELECT profile_id FROM usage_events", [], |row| row.get(0))
            .expect("usage event");
        assert_eq!(usage_profile, Some(profile_id.to_string()));

        // New records are accepted again.
        store
            .insert_profile(&test_profile(directory.path(), "personal", "codex"))
            .expect("new profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        store
            .insert_session(&discovered_session(
                &workspace,
                &profile,
                "claude-2",
                ResumeCapability::Native,
            ))
            .expect("new session");
    }

    #[test]
    fn failed_v5_migration_leaves_the_database_at_v4() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let now = Utc::now().to_rfc3339();
        let connection = Connection::open(&path).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (4);
                 CREATE TABLE profiles (
                   id TEXT PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                   display_name TEXT NOT NULL, provider_id TEXT NOT NULL,
                   description TEXT, provider_home TEXT NOT NULL UNIQUE,
                   account_fingerprint TEXT, auth_state TEXT NOT NULL,
                   created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_used_at TEXT,
                   agent_id TEXT NOT NULL DEFAULT 'codex', model_provider_id TEXT,
                   model_preference TEXT, agent_home TEXT
                 );",
            )
            .expect("legacy v4 schema");
        // The rebuilt table requires an agent home, so this row makes the v5
        // step fail part-way.
        connection
            .execute(
                "INSERT INTO profiles VALUES (?1, 'work', 'Work', 'claude', NULL, ?2, NULL, 'signed_out', ?3, ?3, NULL, 'claude', NULL, NULL, NULL)",
                params![
                    Uuid::new_v4().to_string(),
                    directory.path().join("home").to_string_lossy(),
                    now
                ],
            )
            .expect("profile");
        drop(connection);

        assert!(Store::open(&path).is_err());
        let connection = Connection::open(&path).expect("database after failure");
        let version: i64 = connection
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 4);
        assert!(table_has_column(&connection, "profiles", "provider_id").unwrap());
        let profiles: i64 = connection
            .query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0))
            .expect("profiles");
        assert_eq!(profiles, 1);
    }

    #[test]
    fn migrates_v3_session_titles_and_reopen_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("legacy database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (3);
                 CREATE TABLE sessions (
                   id TEXT PRIMARY KEY, provider_session_id TEXT, workspace_id TEXT NOT NULL,
                   profile_id TEXT, agent_id TEXT NOT NULL, model_provider_id TEXT, model TEXT,
                   started_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                   resume_capability TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
                   continuity TEXT NOT NULL DEFAULT 'unknown'
                 );",
            )
            .expect("v3 schema");
        drop(connection);

        Store::open(&path).expect("v3 migration");
        Store::open(&path).expect("idempotent reopen");
        let connection = Connection::open(&path).expect("migrated database");
        assert!(table_has_column(&connection, "sessions", "title").expect("title column"));
        let version: i64 = connection
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("future database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (99);",
            )
            .expect("future schema");
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(ContextWakeError::InvalidData(message)) if message.contains("not supported")
        ));
    }

    #[test]
    fn newer_schema_is_refused_without_changing_the_database() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("future database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (6);",
            )
            .expect("future schema");
        drop(connection);

        assert!(Store::open(&path).is_err());
        let connection = Connection::open(&path).expect("database after refusal");
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .expect("tables");
        assert_eq!(tables, 1, "no table was created in a newer database");
    }

    #[test]
    fn duplicate_schema_metadata_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("invalid database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (4), (4);",
            )
            .expect("duplicate metadata");
        drop(connection);

        assert!(matches!(
            Store::open(&path),
            Err(ContextWakeError::InvalidData(message)) if message.contains("exactly one row")
        ));
    }

    #[test]
    fn failed_v3_migration_rolls_back_schema_version() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let connection = Connection::open(&path).expect("broken database");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (version INTEGER NOT NULL);
                 INSERT INTO schema_meta(version) VALUES (3);
                 CREATE VIEW sessions AS SELECT 1 AS id;",
            )
            .expect("broken v3 schema");
        drop(connection);

        assert!(Store::open(&path).is_err());
        let connection = Connection::open(&path).expect("database after failure");
        let version: i64 = connection
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .expect("version after rollback");
        assert_eq!(version, 3);
        assert!(!table_has_column(&connection, "sessions", "title").expect("no title column"));
    }

    #[test]
    fn checkpoint_and_handoff_index_rows_commit_with_their_usage_event() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        Connection::open(store.path())
            .expect("connection")
            .execute_batch(
                "CREATE TRIGGER reject_usage BEFORE INSERT ON usage_events
                 BEGIN SELECT RAISE(ABORT, 'usage events unavailable'); END;",
            )
            .expect("trigger");

        let checkpoint = store.insert_checkpoint_index(
            Uuid::new_v4(),
            workspace.id,
            None,
            None,
            None,
            None,
            None,
            "Objective",
            &directory.path().join("checkpoint.json"),
            RedactionStatus::Clean,
            Utc::now(),
        );
        assert!(checkpoint.is_err());
        assert!(
            store.list_checkpoint_paths().unwrap().is_empty(),
            "no index row outlives a failed insert"
        );

        let handoff = store.insert_handoff(&HandoffRecord {
            id: Uuid::new_v4(),
            checkpoint_id: Uuid::new_v4(),
            schema_version: "1.2.0".into(),
            storage_path: directory.path().join("handoff"),
            sha256: "0".repeat(64),
            created_at: Utc::now(),
        });
        assert!(handoff.is_err());
        assert!(store.list_handoffs().unwrap().is_empty());
    }

    #[test]
    fn checkpoint_index_rows_without_files_can_be_listed_and_deleted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let first = test_workspace(directory.path().join("first"), "first");
        let second = test_workspace(directory.path().join("second"), "second");
        store.upsert_workspace(&first).expect("first");
        store.upsert_workspace(&second).expect("second");
        let present = directory.path().join("present.json");
        std::fs::write(&present, "{}").expect("checkpoint file");
        let missing = directory.path().join("missing.json");
        let (older, newer, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now();
        for (id, workspace, path, created_at) in [
            (older, first.id, present, now - chrono::Duration::hours(1)),
            (newer, first.id, missing.clone(), now),
            (
                other,
                second.id,
                directory.path().join("other.json"),
                now - chrono::Duration::minutes(5),
            ),
        ] {
            store
                .insert_checkpoint_index(
                    id,
                    workspace,
                    None,
                    None,
                    None,
                    None,
                    None,
                    "Objective",
                    &path,
                    RedactionStatus::Clean,
                    created_at,
                )
                .expect("index row");
        }

        let entries = store.workspace_checkpoint_index(first.id).expect("entries");
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![newer, older]
        );
        let mut dangling = store
            .missing_checkpoint_files()
            .expect("dangling")
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        dangling.sort();
        let mut expected = vec![newer, other];
        expected.sort();
        assert_eq!(dangling, expected);

        assert_eq!(
            store
                .delete_checkpoint_index(&newer.to_string())
                .expect("delete"),
            missing
        );
        assert!(matches!(
            store.delete_checkpoint_index(&newer.to_string()),
            Err(ContextWakeError::CheckpointNotFound(_))
        ));
        assert_eq!(store.list_checkpoint_index().unwrap().len(), 2);
    }

    #[test]
    fn read_then_write_operations_wait_for_a_concurrent_writer() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let profile = test_profile(directory.path(), "kimi", "kimi");
        store.insert_profile(&profile).expect("profile");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        let session = discovered_session(&workspace, &profile, "kimi-1", ResumeCapability::Unknown);
        let profile_id = profile.id;
        let operations: Vec<Box<dyn FnOnce(Store) -> Result<()> + Send>> = vec![
            Box::new(move |store: Store| store.set_active_profile(profile_id)),
            Box::new(move |store: Store| store.upsert_provider_session(&session).map(|_| ())),
            Box::new(move |store: Store| store.set_active_workspace(workspace.id)),
        ];
        for operation in operations {
            let writer = Connection::open(store.path()).expect("writer");
            writer
                .execute_batch("BEGIN IMMEDIATE;")
                .expect("write lock");
            let pending = std::thread::spawn({
                let store = store.clone();
                move || operation(store)
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            writer.execute_batch("COMMIT;").expect("release");
            pending
                .join()
                .expect("operation thread")
                .expect("the operation waits for the writer instead of failing as locked");
        }
    }

    #[test]
    fn profile_ids_take_precedence_over_matching_names() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let target = test_profile(directory.path(), "target", "codex");
        // A profile name may be any slug, including one that spells another
        // profile's ID.
        let lookalike = Profile {
            id: Uuid::new_v4(),
            name: target.id.to_string(),
            agent_home: directory.path().join("lookalike-home"),
            home_mode: crate::model::ProfileHomeMode::Isolated,
            ..target.clone()
        };
        store.insert_profile(&lookalike).expect("lookalike");
        store.insert_profile(&target).expect("target");
        for reference in [target.id.to_string(), target.id.to_string().to_uppercase()] {
            assert_eq!(
                store.profile(&reference).unwrap().id,
                target.id,
                "{reference}"
            );
        }
        assert_eq!(store.profile("TARGET").unwrap().id, target.id);
        assert_eq!(
            store.profile(&lookalike.id.to_string()).unwrap().id,
            lookalike.id
        );
    }

    #[test]
    fn provider_session_ids_shared_by_several_sessions_are_refused() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Store::open(directory.path().join("state.db")).expect("store");
        let first = test_profile(directory.path(), "first", "kiro");
        let second = test_profile(directory.path(), "second", "kiro");
        store.insert_profile(&first).expect("first");
        store.insert_profile(&second).expect("second");
        let workspace = test_workspace(directory.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        // Agents without profile isolation report the same session to both.
        let a = discovered_session(&workspace, &first, "shared", ResumeCapability::Native);
        let b = discovered_session(&workspace, &second, "shared", ResumeCapability::Native);
        store.upsert_provider_session(&a).expect("first sync");
        store.upsert_provider_session(&b).expect("second sync");

        let error = store.session("shared").expect_err("ambiguous");
        let ContextWakeError::AmbiguousReference(message) = &error else {
            panic!("expected an ambiguity error, got {error}");
        };
        assert!(message.contains(&a.id.to_string()), "{message}");
        assert!(message.contains(&b.id.to_string()), "{message}");
        assert!(store.archive_session("shared", true).is_err());
        assert_eq!(store.list_sessions(false).unwrap().len(), 2);
        assert_eq!(
            store.session(&a.id.to_string()).unwrap().profile_id,
            Some(first.id)
        );
    }

    #[test]
    fn managed_paths_recorded_under_a_previous_data_directory_follow_the_database() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_data = root.path().join("agentdeck").join("data");
        let new_data = root.path().join("contextwake").join("data");
        std::fs::create_dir_all(&old_data).expect("old data directory");
        let store = Store::open(old_data.join("state.sqlite3")).expect("store");
        let mut profile = test_profile(root.path(), "work", "codex");
        profile.agent_home = old_data
            .join(AGENT_HOMES_DIR)
            .join("codex")
            .join(profile.id.to_string());
        store.insert_profile(&profile).expect("profile");
        let custom = test_profile(root.path(), "custom", "claude");
        store.insert_profile(&custom).expect("custom-home profile");
        let workspace = test_workspace(root.path().join("workspace"), "workspace");
        store.upsert_workspace(&workspace).expect("workspace");
        let (moved, unmoved) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [moved, unmoved] {
            store
                .insert_checkpoint_index(
                    id,
                    workspace.id,
                    None,
                    None,
                    None,
                    None,
                    None,
                    "Objective",
                    &old_data.join(CHECKPOINTS_DIR).join(format!("{id}.json")),
                    RedactionStatus::Clean,
                    Utc::now(),
                )
                .expect("checkpoint");
        }
        let handoff_id = Uuid::new_v4();
        let handoff = HandoffRecord {
            id: handoff_id,
            checkpoint_id: moved,
            schema_version: "1.2.0".into(),
            storage_path: old_data.join(HANDOFFS_DIR).join(handoff_id.to_string()),
            sha256: "0".repeat(64),
            created_at: Utc::now(),
        };
        store.insert_handoff(&handoff).expect("handoff");
        drop(store);
        std::fs::create_dir_all(&profile.agent_home).expect("agent home");
        std::fs::create_dir_all(old_data.join(CHECKPOINTS_DIR)).expect("checkpoints");
        std::fs::write(
            old_data.join(CHECKPOINTS_DIR).join(format!("{moved}.json")),
            "{}",
        )
        .expect("checkpoint file");
        std::fs::create_dir_all(&handoff.storage_path).expect("handoff directory");

        std::fs::create_dir_all(root.path().join("contextwake")).expect("new root");
        std::fs::rename(&old_data, &new_data).expect("move the data directory");
        let store = Store::open(new_data.join("state.sqlite3")).expect("reopen");
        assert_eq!(
            store.profile("work").unwrap().agent_home,
            new_data
                .join(AGENT_HOMES_DIR)
                .join("codex")
                .join(profile.id.to_string())
        );
        assert_eq!(
            store.checkpoint_path(&moved.to_string()).unwrap(),
            new_data.join(CHECKPOINTS_DIR).join(format!("{moved}.json"))
        );
        assert_eq!(
            store.handoff(&handoff.id.to_string()).unwrap().storage_path,
            new_data.join(HANDOFFS_DIR).join(handoff.id.to_string())
        );
        // Paths outside the managed layout, and entries that did not move,
        // are left as recorded.
        assert_eq!(
            store.profile("custom").unwrap().agent_home,
            custom.agent_home
        );
        assert_eq!(
            store.checkpoint_path(&unmoved.to_string()).unwrap(),
            old_data
                .join(CHECKPOINTS_DIR)
                .join(format!("{unmoved}.json"))
        );
    }
}
