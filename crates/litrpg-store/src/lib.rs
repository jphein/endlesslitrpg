//! SQLite persistence. The only crate in the workspace that writes state.

pub mod alias;
pub mod chapters;
pub mod heartbeat;
pub mod ledger;
pub mod library;
pub mod migrations;
pub mod story;

pub use alias::Alias;
pub use chapters::{ChapterRow, NewChapter};
pub use heartbeat::EngineHeartbeat;
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
    /// A single-field setter was handed an empty string. Rejected rather than written
    /// because these setters exist to *correct* a field, and "" is never a correction —
    /// while an empty `protagonist` silently changes ledger validation (it drops out of
    /// `known_subjects`, so every delta about the protagonist becomes an unknown subject).
    #[error("{field} cannot be empty")]
    EmptyField { field: &'static str },
    /// The database was written by a newer build. Refused rather than opened, because the
    /// alternative is an old binary quietly operating on a schema it does not know.
    #[error("database schema is version {found}, but this binary supports {supported}; rebuild")]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("{name:?} cannot be an alias of itself")]
    SelfAlias { name: String },

    /// A reserved role given a kind that would make it a person, or a person given a kind
    /// that would stop them being one.
    #[error(
        "{speaker:?} is a reserved role and cannot have kind {kind:?} — `kind` is the only          authority on whether a row can hold stats, so this would have made a voice into a          character"
    )]
    ReservedKindMismatch { speaker: String, kind: String },

    #[error(
        "{speaker:?} is a character and cannot have kind {kind:?} — that kind marks a row as          not-a-person, so their stat changes would stop being accepted"
    )]
    PersonGivenRoleKind { speaker: String, kind: String },

    /// A migration failed, naming which one.
    ///
    /// SQLite's bare "UNIQUE constraint failed" says nothing about *where*, and a schema
    /// error you cannot locate costs more than the error itself.
    ///
    /// `detail` is a *summary*, and the rusqlite error is deliberately **not** chained as a
    /// `#[source]`. `rusqlite::Error::SqlInputError` includes the entire statement text in its
    /// `Display`, so chaining it printed the whole migration file — twice, once per level of
    /// the error chain — and buried the migration's name under eighty lines of SQL. Naming the
    /// migration was the entire point; an unreadable error that happens to contain the name is
    /// no better than the bare one it replaced. The offending statement is in the named file,
    /// which also carries the diagnostic query.
    #[error("migration {name} (index {index}) failed: {detail}")]
    Migration {
        name: &'static str,
        index: usize,
        detail: String,
    },
    #[error("{0} is not in the cast")]
    UnknownSpeaker(String),
    #[error("chapter {chapter} segment {idx} has an unrecognised kind {value:?}")]
    InvalidSegmentKind {
        chapter: u32,
        idx: u32,
        value: String,
    },
    #[error("delta rejected: {0}")]
    Rejected(String),
}

pub type Result<T> = core::result::Result<T, StoreError>;

/// Summarise a migration failure without reproducing the statement.
///
/// `SqlInputError` carries the full SQL in its `Display`; a migration file with a documented
/// header is dozens of lines, and printing it makes the message unreadable at exactly the
/// moment someone needs to read it.
fn migration_detail(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqlInputError { msg, offset, .. } => {
            format!("{msg} (at offset {offset} of the migration)")
        }
        other => other.to_string(),
    }
}

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
        // A database newer than this binary would otherwise open silently: every
        // migration index is below `current`, so the loop applies nothing and the pragma
        // update is skipped, and an old binary proceeds to operate on a schema it does
        // not know. Additive migrations make that harmless right up until one drops a
        // column, at which point the symptom is an obscure SQL error rather than "your
        // binary is too old". Mixed binaries are the normal state here — the engine runs
        // under systemd while the CLI is rebuilt against the same file.
        if current > TARGET_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: current,
                supported: TARGET_VERSION,
            });
        }
        for (i, m) in MIGRATIONS.iter().enumerate() {
            let version = i as i64;
            if version < current {
                continue;
            }
            self.conn
                .execute_batch(m.sql)
                .map_err(|e| StoreError::Migration {
                    name: m.name,
                    index: i,
                    detail: migration_detail(&e),
                })?;
            // Steps that need the *rule* rather than just DDL. Keyed on the migration's
            // name, not its index, so renumbering cannot silently attach a step to the
            // wrong migration.
            self.post_migration(m.name)?;
        }
        if current < TARGET_VERSION {
            self.conn
                .pragma_update(None, "user_version", TARGET_VERSION)?;
        }
        Ok(())
    }

    /// Work a migration cannot express in SQL without restating a rule Rust owns.
    ///
    /// Writing `lower(trim(speaker))` in the `.sql` would have been a second expression of
    /// `litrpg_core::speaker::identity_key` — agreeing today, free to drift tomorrow. So the
    /// DDL adds columns and the values come from the owner.
    fn post_migration(&self, name: &str) -> Result<()> {
        match name {
            "006_cast_identity_key" => self.backfill_cast_identity_keys(),
            "007_subject_alias" => self.seed_known_aliases(),
            _ => Ok(()),
        }
    }

    /// The domain tables.
    ///
    /// Excludes SQLite's own bookkeeping and anything prefixed `_`, which is the
    /// convention here for internal tables a migration leaves behind — migration 004
    /// preserves the media paths it dropped in `_audit_004_paths`, because the
    /// stale-path bug that motivated the drop was diagnosable *precisely* because
    /// those values were on record. Without this filter every such migration would
    /// churn a test that is really asking "do the domain tables exist".
    ///
    /// `substr(name, 1, 1)` rather than `NOT LIKE '_%'` because `_` is a LIKE
    /// wildcard, so the obvious spelling would exclude every table.
    /// Wind `user_version` back, so a test can make a migration re-run and fail.
    ///
    /// Not a supported operation and deliberately not part of the normal surface: replaying a
    /// migration against a database that already holds its objects is exactly the situation
    /// migrations do not handle. It exists so the *error path* can be exercised, because an
    /// error message nobody has seen is not yet evidence.
    #[doc(hidden)]
    pub fn conn_pragma_for_test(&self, version: i64) {
        let _ = self.conn.pragma_update(None, "user_version", version);
    }

    pub fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND substr(name, 1, 1) <> '_'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Run a statement directly. **Tests only** — it exists so a test can simulate the
    /// hand-edit or bad migration that strict decoding defends against, which is
    /// otherwise unreachable through the safe API.
    #[doc(hidden)]
    pub fn raw_execute_for_tests(&self, sql: &str) -> Result<usize> {
        Ok(self.conn.execute(sql, [])?)
    }
}
