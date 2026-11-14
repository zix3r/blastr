//! On-disk cache for Blastr.
//!
//! `blastr-store` owns a single SQLite database per user, kept outside the
//! repository being analysed. It records the schema version used by this build
//! and knows where the cache and configuration directories live.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

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
        write_raw_schema(&path, "0", Some("old_data"));
        assert!(table_exists(&path, "old_data"));

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(!table_exists(&path, "old_data"));
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
}
