//! The singleton `story` row: title, protagonist, prompt provenance, arc outline.

use rusqlite::params;

use crate::{Result, Store, StoreError, now_ms};

/// Input for creating or updating the story row.
///
/// Deliberately carries no `id` (the table is a singleton) and no `updated_at`
/// (the store stamps its own time, as every other write path here does), so a
/// caller cannot invent either. Nor does it carry `arc_outline_md` — see
/// [`Store::upsert_story`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewStory {
    pub title: String,
    pub protagonist: String,
    pub prompt_path: String,
    pub prompt_hash: String,
    pub target_words: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryRow {
    pub title: String,
    pub protagonist: String,
    pub prompt_path: String,
    pub prompt_hash: String,
    /// Engine-owned narrative state. Written only by [`Store::set_arc_outline`].
    pub arc_outline_md: String,
    pub target_words: u32,
    pub updated_at: i64,
}

const STORY_COLUMNS: &str =
    "title, protagonist, prompt_path, prompt_hash, arc_outline_md, target_words, updated_at";

impl Store {
    pub fn story(&self) -> Result<Option<StoryRow>> {
        let sql = format!("SELECT {STORY_COLUMNS} FROM story ORDER BY id LIMIT 1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Ok(Some(StoryRow {
                title: r.get(0)?,
                protagonist: r.get(1)?,
                prompt_path: r.get(2)?,
                prompt_hash: r.get(3)?,
                arc_outline_md: r.get(4)?,
                target_words: r.get::<_, i64>(5)? as u32,
                updated_at: r.get(6)?,
            })),
            None => Ok(None),
        }
    }

    /// Insert only if the table is empty; reports whether it inserted.
    ///
    /// One atomic statement rather than a caller-side check-then-act, so `init`
    /// cannot race itself and the emptiness rule lives in exactly one place.
    pub fn insert_story_if_absent(&self, s: &NewStory) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT INTO story
                (title, protagonist, prompt_path, prompt_hash, target_words, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE NOT EXISTS (SELECT 1 FROM story)",
            params![
                s.title,
                s.protagonist,
                s.prompt_path,
                s.prompt_hash,
                s.target_words,
                now_ms()
            ],
        )?;
        Ok(n > 0)
    }

    /// Overwrite the caller-owned fields, inserting the row if absent.
    ///
    /// # `arc_outline_md` is preserved, deliberately
    ///
    /// The `UPDATE` names its columns explicitly and **omits `arc_outline_md`**. That
    /// omission is the point, not an oversight: the outline is engine-owned narrative
    /// state that a caller of this function has no value for. `litrpg init --force`
    /// run at chapter 60 — a plausible way to fix a title or repoint a path — would
    /// otherwise silently erase the story's arc. That is the same shape as the
    /// `known_subjects` and `rejected_count` bugs: a write path quietly destroying
    /// state a *different* subsystem depends on.
    ///
    /// Keeping the preservation here rather than in the caller also means the rule
    /// lives in one crate instead of being knowledge two crates must share.
    pub fn upsert_story(&self, s: &NewStory) -> Result<()> {
        if self.insert_story_if_absent(s)? {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE story SET
                 title = ?1,
                 protagonist = ?2,
                 prompt_path = ?3,
                 prompt_hash = ?4,
                 target_words = ?5,
                 updated_at = ?6",
            params![
                s.title,
                s.protagonist,
                s.prompt_path,
                s.prompt_hash,
                s.target_words,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Replace the arc outline. Engine-owned; nothing else should call this.
    ///
    /// Errors rather than no-ops when there is no story row, because a silent
    /// zero-row write is how narrative state goes missing without a trace.
    pub fn set_arc_outline(&self, outline: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE story SET arc_outline_md = ?1, updated_at = ?2",
            params![outline, now_ms()],
        )?;
        if n == 0 {
            return Err(StoreError::NoStoryRow);
        }
        Ok(())
    }
}
