//! Chapter and segment persistence.

use std::time::{SystemTime, UNIX_EPOCH};

use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use rusqlite::params;

use crate::{Result, Store, StoreError};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn kind_str(k: SpeakerKind) -> &'static str {
    match k {
        SpeakerKind::Narrator => "narrator",
        SpeakerKind::Character => "character",
        SpeakerKind::System => "system",
    }
}

fn kind_from_str(s: &str) -> SpeakerKind {
    match s {
        "character" => SpeakerKind::Character,
        "system" => SpeakerKind::System,
        _ => SpeakerKind::Narrator,
    }
}

/// Input for inserting a chapter. Audio is attached separately, because text
/// ships even when rendering fails (spec §10).
#[derive(Debug, Clone)]
pub struct NewChapter {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub state_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct ChapterRow {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub pcm_path: Option<String>,
    pub mp3_path: Option<String>,
    pub duration_ms: u32,
    pub has_audio: bool,
    pub state_dirty: bool,
}

impl Store {
    pub fn insert_chapter(&self, ch: &NewChapter) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chapters (number, title, text_md, prompt_hash, state_dirty, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ch.number,
                ch.title,
                ch.text_md,
                ch.prompt_hash,
                ch.state_dirty as i64,
                now_ms()
            ],
        )?;
        Ok(())
    }

    fn row_to_chapter(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChapterRow> {
        Ok(ChapterRow {
            number: r.get::<_, i64>(0)? as u32,
            title: r.get(1)?,
            text_md: r.get(2)?,
            prompt_hash: r.get(3)?,
            pcm_path: r.get(4)?,
            mp3_path: r.get(5)?,
            duration_ms: r.get::<_, i64>(6)? as u32,
            has_audio: r.get::<_, i64>(7)? != 0,
            state_dirty: r.get::<_, i64>(8)? != 0,
        })
    }

    const CHAPTER_COLUMNS: &'static str = "number, title, text_md, prompt_hash, pcm_path, mp3_path, duration_ms, has_audio, state_dirty";

    pub fn chapter(&self, number: u32) -> Result<ChapterRow> {
        let sql = format!(
            "SELECT {} FROM chapters WHERE number = ?1",
            Self::CHAPTER_COLUMNS
        );
        self.conn
            .query_row(&sql, params![number], Self::row_to_chapter)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StoreError::ChapterNotFound(number),
                other => StoreError::Sqlite(other),
            })
    }

    pub fn chapters_since(&self, after: u32) -> Result<Vec<ChapterRow>> {
        let sql = format!(
            "SELECT {} FROM chapters WHERE number > ?1 ORDER BY number",
            Self::CHAPTER_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![after], Self::row_to_chapter)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn latest_number(&self) -> Result<u32> {
        let n: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(number), 0) FROM chapters", [], |r| {
                    r.get(0)
                })?;
        Ok(n as u32)
    }

    pub fn dirty_chapters(&self) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT number FROM chapters WHERE state_dirty = 1 ORDER BY number")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|n| n as u32)
            .collect())
    }

    /// Attach rendered audio: manifest JSON, per-segment rows, duration, paths.
    /// Replaces any previous segments so a re-render cannot duplicate them.
    ///
    /// Wrapped in a transaction: a partially-attached chapter (`has_audio = 1` with
    /// no segment rows) would make the manifest and the segment table disagree,
    /// and every client derives Range requests from that pair.
    pub fn attach_audio(
        &self,
        number: u32,
        manifest: &Manifest,
        pcm_path: &str,
        mp3_path: &str,
    ) -> Result<()> {
        let chapter_id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM chapters WHERE number = ?1",
                params![number],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StoreError::ChapterNotFound(number),
                other => StoreError::Sqlite(other),
            })?;

        let json = serde_json::to_string(manifest)?;

        // `unchecked_transaction` is the `&self` form; it rolls back on drop, so an
        // early `?` below cannot leave the chapter half-attached.
        let tx = self.conn.unchecked_transaction()?;

        self.conn.execute(
            "UPDATE chapters
             SET manifest_json = ?1, pcm_path = ?2, mp3_path = ?3,
                 duration_ms = ?4, has_audio = 1
             WHERE id = ?5",
            params![json, pcm_path, mp3_path, manifest.duration_ms, chapter_id],
        )?;

        self.conn.execute(
            "DELETE FROM segments WHERE chapter_id = ?1",
            params![chapter_id],
        )?;

        for s in &manifest.segments {
            self.conn.execute(
                "INSERT INTO segments
                    (chapter_id, idx, speaker, kind, text, voice_ref, start_ms, end_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    chapter_id,
                    s.idx,
                    s.speaker,
                    kind_str(s.kind),
                    s.text,
                    s.voice_ref,
                    s.start_ms,
                    s.end_ms
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn segments(&self, number: u32) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.idx, s.speaker, s.kind, s.text, s.voice_ref, s.start_ms, s.end_ms
             FROM segments s
             JOIN chapters c ON c.id = s.chapter_id
             WHERE c.number = ?1
             ORDER BY s.idx",
        )?;
        let rows = stmt.query_map(params![number], |r| {
            Ok(Segment {
                idx: r.get::<_, i64>(0)? as u32,
                speaker: r.get(1)?,
                kind: kind_from_str(&r.get::<_, String>(2)?),
                text: r.get(3)?,
                voice_ref: r.get(4)?,
                start_ms: r.get::<_, i64>(5)? as u32,
                end_ms: r.get::<_, i64>(6)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
