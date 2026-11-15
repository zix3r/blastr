//! On-disk cache for Blastr.
//!
//! `blastr-store` owns a single SQLite database per user, kept outside the
//! repository being analysed. It records the schema version used by this build
//! and knows where the cache and configuration directories live.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, params};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 2;

/// Statement creating the `graphs` table, also used when an old cache is rebuilt.
const CREATE_GRAPHS: &str = "CREATE TABLE IF NOT EXISTS graphs (\
     key_hash TEXT PRIMARY KEY, \
     key_json TEXT NOT NULL, \
     data BLOB NOT NULL, \
     bytes INTEGER NOT NULL, \
     created_at INTEGER NOT NULL, \
     last_used_at INTEGER NOT NULL)";

/// Errors produced while accessing the cache.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A SQLite operation failed.
    #[error("cache database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A filesystem operation failed.
    #[error("cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The database was written by a newer version of Blastr.
    #[error(
        "the cache was written by a newer version of blastr (schema {found}, this build supports {supported})"
    )]
    NewerSchema {
        /// The schema version recorded in the database.
        found: u32,
        /// The schema version supported by this build.
        supported: u32,
    },
    /// No home directory could be determined for the current user.
    #[error("could not determine the user's cache and configuration directories")]
    NoUserDirs,
}

/// Where Blastr keeps its files for the current user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Cache directory: the database and revision snapshots.
    pub cache_dir: PathBuf,
    /// Configuration directory: the user configuration file.
    pub config_dir: PathBuf,
}

impl Paths {
    /// Path of the cache database inside `cache_dir` (file name `blastr.db`).
    pub fn database(&self) -> PathBuf {
        self.cache_dir.join("blastr.db")
    }

    /// Directory holding materialised revisions inside `cache_dir`.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.cache_dir.join("snapshots")
    }

    /// Directory used as `CARGO_TARGET_DIR` for analysis builds.
    pub fn target_dir(&self) -> PathBuf {
        self.cache_dir.join("target")
    }
}

/// One cached revision graph, without its encoded bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEntry {
    /// Stable hash identifying the analysis key.
    pub key_hash: String,
    /// Canonical JSON of the analysis key.
    pub key_json: String,
    /// Size of the encoded graph in bytes.
    pub bytes: u64,
    /// Creation time, in seconds since the Unix epoch.
    pub created_at: i64,
    /// Time of the last read, in seconds since the Unix epoch.
    pub last_used_at: i64,
}

/// Returns the per-user directories, from the `directories` crate
/// (`ProjectDirs::from("", "", "blastr")`). Does not create them.
pub fn paths() -> Result<Paths, Error> {
    let dirs = ProjectDirs::from("", "", "blastr").ok_or(Error::NoUserDirs)?;
    Ok(Paths {
        cache_dir: dirs.cache_dir().to_path_buf(),
        config_dir: dirs.config_dir().to_path_buf(),
    })
}

/// An open cache database.
pub struct Store {
    connection: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store { .. }")
    }
}

impl Store {
    /// Opens the database at `path`, creating the file and its parent
    /// directories if needed.
    pub fn open(path: &Path) -> Result<Store, Error> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        init(&mut connection)?;
        Ok(Store { connection })
    }

    /// Opens an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Store, Error> {
        let mut connection = Connection::open_in_memory()?;
        init(&mut connection)?;
        Ok(Store { connection })
    }

    /// The schema version recorded in the database.
    pub fn schema_version(&self) -> Result<u32, Error> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match value {
            Some(text) => Ok(parse_version(&text)?),
            None => Ok(SCHEMA_VERSION),
        }
    }

    /// Stored bytes for a key; also updates `last_used_at`.
    ///
    /// A missing key is not an error and returns `None`.
    pub fn get_graph(&self, key_hash: &str) -> Result<Option<Vec<u8>>, Error> {
        let data: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT data FROM graphs WHERE key_hash = ?1",
                [key_hash],
                |row| row.get(0),
            )
            .optional()?;
        if data.is_some() {
            self.connection.execute(
                "UPDATE graphs SET last_used_at = ?2 WHERE key_hash = ?1",
                params![key_hash, now_secs()],
            )?;
        }
        Ok(data)
    }

    /// Inserts the graph for `key_hash`, replacing any existing row.
    ///
    /// Replacing resets `created_at` as well as the stored data.
    pub fn put_graph(&self, key_hash: &str, key_json: &str, data: &[u8]) -> Result<(), Error> {
        let now = now_secs();
        // A slice length always fits in i64 on the targets Blastr supports.
        let bytes = data.len() as i64;
        self.connection.execute(
            "INSERT OR REPLACE INTO graphs \
             (key_hash, key_json, data, bytes, created_at, last_used_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![key_hash, key_json, data, bytes, now],
        )?;
        Ok(())
    }

    /// All entries, most recently used first.
    ///
    /// Ties on `last_used_at` are ordered by `key_hash` ascending. The encoded
    /// graph itself is not loaded.
    pub fn list_graphs(&self) -> Result<Vec<GraphEntry>, Error> {
        let mut statement = self.connection.prepare(
            "SELECT key_hash, key_json, bytes, created_at, last_used_at FROM graphs \
             ORDER BY last_used_at DESC, key_hash ASC",
        )?;
        let rows = statement.query_map([], |row| {
            let bytes: i64 = row.get(2)?;
            Ok(GraphEntry {
                key_hash: row.get(0)?,
                key_json: row.get(1)?,
                bytes: u64::try_from(bytes).unwrap_or(0),
                created_at: row.get(3)?,
                last_used_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sum of `bytes` over all entries; 0 when the cache is empty.
    pub fn total_size(&self) -> Result<u64, Error> {
        let total: i64 =
            self.connection
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM graphs", [], |row| {
                    row.get(0)
                })?;
        Ok(u64::try_from(total).unwrap_or(0))
    }

    /// Removes every entry and shrinks the database file; returns how many
    /// entries were removed.
    pub fn clear(&self) -> Result<u64, Error> {
        let removed = self.connection.execute("DELETE FROM graphs", [])?;
        // `VACUUM` returns no rows and cannot run inside a transaction.
        self.connection.execute("VACUUM", [])?;
        Ok(u64::try_from(removed).unwrap_or(0))
    }
}

/// Creates the schema bookkeeping table and reconciles the stored version.
fn init(connection: &mut Connection) -> Result<(), Error> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;

    let recorded: Option<String> = transaction
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    match recorded {
        None => set_version(&transaction, SCHEMA_VERSION)?,
        Some(text) => {
            let found = parse_version(&text)?;
            if found > SCHEMA_VERSION {
                return Err(Error::NewerSchema {
                    found,
                    supported: SCHEMA_VERSION,
                });
            }
            if found < SCHEMA_VERSION {
                drop_all_tables_except_meta(&transaction)?;
                set_version(&transaction, SCHEMA_VERSION)?;
            }
        }
    }

    transaction.execute(CREATE_GRAPHS, [])?;

    transaction.commit()?;
    Ok(())
}

/// Stores `version` as the schema version.
fn set_version(connection: &Connection, version: u32) -> Result<(), Error> {
    connection.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
        [version.to_string()],
    )?;
    Ok(())
}

/// Removes every user table except `meta`, because an older cache can be rebuilt.
fn drop_all_tables_except_meta(connection: &Connection) -> Result<(), Error> {
    let names: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> 'meta'",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for name in names {
        let quoted = name.replace('"', "\"\"");
        connection.execute(&format!("DROP TABLE IF EXISTS \"{quoted}\""), [])?;
    }
    Ok(())
}

/// Parses a version string stored in the `meta` table.
fn parse_version(text: &str) -> Result<u32, rusqlite::Error> {
    text.trim().parse::<u32>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

/// The current time in seconds since the Unix epoch.
///
/// A clock set before the epoch is reported as the epoch itself.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_raw_schema(path: &Path, version: &str, extra_table: Option<&str>) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
                [version],
            )
            .unwrap();
        if let Some(table) = extra_table {
            connection
                .execute(&format!("CREATE TABLE \"{table}\" (id INTEGER)"), [])
                .unwrap();
        }
    }

    fn write_graphs_row(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection.execute(CREATE_GRAPHS, []).unwrap();
        connection
            .execute(
                "INSERT INTO graphs (key_hash, key_json, data, bytes, created_at, last_used_at) \
                 VALUES ('old', '{}', x'00', 1, 1, 1)",
                [],
            )
            .unwrap();
    }

    fn table_exists(path: &Path, table: &str) -> bool {
        let connection = Connection::open(path).unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        count > 0
    }

    #[test]
    fn opening_new_path_creates_file_and_sets_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        }
        assert!(path.is_file());
    }

    #[test]
    fn opening_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("blastr.db");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(path.is_file());
    }

    #[test]
    fn opening_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        let newer = SCHEMA_VERSION + 7;
        write_raw_schema(&path, &newer.to_string(), None);

        match Store::open(&path) {
            Err(Error::NewerSchema { found, supported }) => {
                assert_eq!(found, newer);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected NewerSchema, got {other:?}"),
        }

        let connection = Connection::open(&path).unwrap();
        let stored: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, newer.to_string());
    }

    #[test]
    fn older_schema_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        write_raw_schema(&path, "1", Some("old_data"));
        assert!(table_exists(&path, "old_data"));

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(!table_exists(&path, "old_data"));
    }

    #[test]
    fn older_schema_with_graphs_is_empty_and_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        write_raw_schema(&path, "1", None);
        write_graphs_row(&path);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 2);
        assert!(store.list_graphs().unwrap().is_empty());
        assert_eq!(store.total_size().unwrap(), 0);
        store.put_graph("new", "{}", b"data").unwrap();
        assert_eq!(store.get_graph("new").unwrap(), Some(b"data".to_vec()));
    }

    #[test]
    fn database_path_is_blastr_db() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            cache_dir: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
        };
        let database = paths.database();
        assert_eq!(
            database.file_name().and_then(|name| name.to_str()),
            Some("blastr.db")
        );
        assert!(database.starts_with(dir.path()));
    }

    #[test]
    fn in_memory_store_reports_schema_version() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn put_graph_then_get_graph_returns_the_same_bytes() {
        let store = Store::open_in_memory().unwrap();
        store
            .put_graph("k1", "{\"key\":1}", b"graph-bytes")
            .unwrap();
        assert_eq!(
            store.get_graph("k1").unwrap(),
            Some(b"graph-bytes".to_vec())
        );
        assert_eq!(store.get_graph("absent").unwrap(), None);
    }

    #[test]
    fn put_graph_twice_replaces_the_row() {
        let store = Store::open_in_memory().unwrap();
        store.put_graph("k1", "json", b"short").unwrap();
        store.put_graph("k1", "json", b"a longer payload").unwrap();

        let entries = store.list_graphs().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key_hash, "k1");
        assert_eq!(entries[0].bytes, b"a longer payload".len() as u64);
        assert_eq!(
            store.get_graph("k1").unwrap(),
            Some(b"a longer payload".to_vec())
        );
    }

    #[test]
    fn list_graphs_and_total_size_describe_the_rows() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.total_size().unwrap(), 0);

        store.put_graph("aaa", "json-a", b"11111").unwrap();
        store.put_graph("bbb", "json-b", b"222").unwrap();

        let mut entries = store.list_graphs().unwrap();
        entries.sort_by(|a, b| a.key_hash.cmp(&b.key_hash));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key_hash, "aaa");
        assert_eq!(entries[0].key_json, "json-a");
        assert_eq!(entries[0].bytes, 5);
        assert_eq!(entries[1].key_hash, "bbb");
        assert_eq!(entries[1].key_json, "json-b");
        assert_eq!(entries[1].bytes, 3);
        assert_eq!(store.total_size().unwrap(), 8);
    }

    #[test]
    fn list_graphs_is_ordered_by_last_use_then_key() {
        let store = Store::open_in_memory().unwrap();
        store.put_graph("aaa", "json-a", b"1").unwrap();
        store.put_graph("bbb", "json-b", b"2").unwrap();
        store
            .connection
            .execute(
                "UPDATE graphs SET last_used_at = 10 WHERE key_hash = 'aaa'",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE graphs SET last_used_at = 20 WHERE key_hash = 'bbb'",
                [],
            )
            .unwrap();
        let keys: Vec<String> = store
            .list_graphs()
            .unwrap()
            .into_iter()
            .map(|entry| entry.key_hash)
            .collect();
        assert_eq!(keys, vec!["bbb".to_string(), "aaa".to_string()]);
    }

    #[test]
    fn clear_removes_rows_and_leaves_the_store_usable() {
        let store = Store::open_in_memory().unwrap();
        store.put_graph("aaa", "json-a", b"12345").unwrap();
        store.put_graph("bbb", "json-b", b"123").unwrap();

        assert_eq!(store.clear().unwrap(), 2);
        assert!(store.list_graphs().unwrap().is_empty());
        assert_eq!(store.total_size().unwrap(), 0);

        store.put_graph("ccc", "json-c", b"abc").unwrap();
        assert_eq!(store.get_graph("ccc").unwrap(), Some(b"abc".to_vec()));
    }

    #[test]
    fn graphs_survive_closing_and_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blastr.db");
        {
            let store = Store::open(&path).unwrap();
            store.put_graph("k1", "json", b"persisted").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_graph("k1").unwrap(), Some(b"persisted".to_vec()));
        let entries = store.list_graphs().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key_json, "json");
    }

    #[test]
    fn snapshots_and_target_dirs_live_in_the_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            cache_dir: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
        };
        let snapshots = paths.snapshots_dir();
        let target = paths.target_dir();
        assert!(snapshots.starts_with(&paths.cache_dir));
        assert!(target.starts_with(&paths.cache_dir));
        assert!(snapshots.ends_with("snapshots"));
        assert!(target.ends_with("target"));
    }
}
