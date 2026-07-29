//! SQLite persistence. The only crate in the workspace that writes state.

pub mod chapters;
pub mod ledger;
pub mod library;
pub mod migrations;
pub mod story;

pub use chapters::{ChapterRow, NewChapter};
pub use ledger::{CastRow, NoteRow, REWOUND_REASON};
pub use library::{LEVEL_CHAPTER, LoreRow, SummaryRow};
pub use story::{NewStory, StoryRow};

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use thiserror::Error;

use migrations::{MIGRATIONS, TARGET_VERSION};

/// Unix milliseconds. One definition, so every write path stamps time the same
/// way and no caller gets to supply its own.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("chapter {0} not found")]
    ChapterNotFound(u32),
    #[error("chapter {number} manifest is invalid: {why}")]
    InvalidManifest { number: u32, why: &'static str },
    #[error("no story row exists; run `litrpg init` first")]
    NoStoryRow,
    #[error("delta rejected: {0}")]
    Rejected(String),
}

pub type Result<T> = core::result::Result<T, StoreError>;

pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.configure(true)?;
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.configure(false)?;
        store.migrate()?;
        Ok(store)
    }

    fn configure(&self, on_disk: bool) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        if on_disk {
            // WAL is invalid for :memory: databases.
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Apply any migrations the database has not yet seen. Idempotent.
    pub fn migrate(&self) -> Result<()> {
        let current = self.schema_version()?;
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let version = i as i64;
            if version < current {
                continue;
            }
            self.conn.execute_batch(sql)?;
        }
        if current < TARGET_VERSION {
            self.conn
                .pragma_update(None, "user_version", TARGET_VERSION)?;
        }
        Ok(())
    }

    pub fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
