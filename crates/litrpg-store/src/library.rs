//! Retrieval surface for the engine's prompt assembly: lore and summaries.
//!
//! These are the readers behind `litrpg-engine`'s `Library` port. Their absence was
//! a silent-degradation path rather than an error: with no lore reader, an entry
//! written in chapter 5 could never be retrieved for chapter 6, so §6.3 retrieval
//! would quietly collapse to always-on entries and the story would drift with
//! nothing reporting a fault.

use rusqlite::params;

use crate::{Result, Store};

/// A lorebook entry. Keyword *selection* is deliberately not done here — it is a
/// pure function in `litrpg-ember`, so the store returns every row and matching
/// stays testable without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoreRow {
    pub name: String,
    /// `character` | `place` | `item` | `faction` | `rule`
    pub kind: String,
    /// Comma-separated keywords.
    pub keywords: String,
    pub body_md: String,
    pub priority: i32,
    pub always_on: bool,
    pub updated_chapter: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    /// `0` chapter · `1` arc · `2` book
    pub level: i32,
    pub from_ch: u32,
    pub to_ch: u32,
    pub body_md: String,
}

/// Level of a per-chapter summary.
pub const LEVEL_CHAPTER: i32 = 0;

impl Store {
    /// Every lore row, highest priority first.
    pub fn lore(&self) -> Result<Vec<LoreRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, keywords, body_md, priority, always_on, updated_chapter
             FROM lore ORDER BY priority DESC, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LoreRow {
                name: r.get(0)?,
                kind: r.get(1)?,
                keywords: r.get(2)?,
                body_md: r.get(3)?,
                priority: r.get::<_, i64>(4)? as i32,
                always_on: r.get::<_, i64>(5)? != 0,
                updated_chapter: r.get::<_, i64>(6)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent `limit` chapter summaries, **oldest first** — the order they
    /// belong in a prompt, so no caller has to remember to reverse them.
    pub fn recent_chapter_summaries(&self, limit: usize) -> Result<Vec<SummaryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT level, from_ch, to_ch, body_md FROM summaries
             WHERE level = ?1 ORDER BY to_ch DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![LEVEL_CHAPTER, limit as i64], |r| {
            Ok(SummaryRow {
                level: r.get::<_, i64>(0)? as i32,
                from_ch: r.get::<_, i64>(1)? as u32,
                to_ch: r.get::<_, i64>(2)? as u32,
                body_md: r.get(3)?,
            })
        })?;
        let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        out.reverse();
        Ok(out)
    }

    /// Insert or replace a summary, keyed by `(level, from_ch, to_ch)`.
    ///
    /// Idempotent by design: re-extracting a `state_dirty` chapter must replace its
    /// summary, never append a second one that would then both appear in retrieval.
    pub fn put_summary(&self, level: i32, from_ch: u32, to_ch: u32, body_md: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO summaries (level, from_ch, to_ch, body_md)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(level, from_ch, to_ch) DO UPDATE SET body_md = excluded.body_md",
            params![level, from_ch, to_ch, body_md],
        )?;
        Ok(())
    }

    /// A per-chapter summary covers exactly one chapter.
    pub fn put_chapter_summary(&self, chapter: u32, body_md: &str) -> Result<()> {
        self.put_summary(LEVEL_CHAPTER, chapter, chapter, body_md)
    }

    /// Chapters whose text shipped but whose audio did not (§10), oldest first.
    /// The engine's resume stage retries these rather than rescanning every row.
    pub fn chapters_missing_audio(&self) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT number FROM chapters WHERE has_audio = 0 ORDER BY number")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|n| n as u32)
            .collect())
    }

    /// Chapters whose pass-2 extraction never succeeded, oldest first.
    pub fn dirty_chapter_numbers(&self) -> Result<Vec<u32>> {
        self.dirty_chapters()
    }
}
