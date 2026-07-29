//! Ledger persistence: append-with-validation, snapshot, rewind.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use litrpg_core::ledger::{LedgerEntry, Op, StateSnapshot, fold};
use litrpg_core::validate::{Delta, Rejection, validate_delta};
use rusqlite::params;

use crate::{Result, Store};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn op_str(op: Op) -> &'static str {
    match op {
        Op::Set => "set",
        Op::Add => "add",
        Op::Sub => "sub",
    }
}

/// Marker written into `reason` when a rewind deactivates a row. Distinct from a
/// validation rejection: a rewind is deliberate, so it must never inflate the
/// rejection metrics that signal prompt drift.
pub const REWOUND_REASON: &str = "rewound";

/// Strict on purpose. An unrecognized op must **not** default to `Set`: that is
/// the most destructive possible fallback — a corrupted `add` would turn
/// "+5 gold" into "gold = 5" absolutely, and the fold would report nothing.
/// Returning `None` lets [`Store::entries`] skip the row and record an anomaly.
///
/// The realistic trigger is not this crate's writer but a hand-edit
/// (`sqlite3 story.db "UPDATE ledger SET ..."`) or a future migration.
fn op_from_str(s: &str) -> Option<Op> {
    match s {
        "set" => Some(Op::Set),
        "add" => Some(Op::Add),
        "sub" => Some(Op::Sub),
        _ => None,
    }
}

/// A persisted speaker → voice assignment. Voices are assigned on a character's
/// first appearance and kept, which is what makes a cast feel like continuity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastRow {
    pub speaker: String,
    pub voice_ref: String,
    pub kind: String,
    pub first_chapter: u32,
}

/// A director note. `consumed_chapter` is `None` until an engine cycle folds it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow {
    pub id: i64,
    pub body: String,
    pub source: String,
    pub created_at: i64,
    pub consumed_chapter: Option<u32>,
}

impl Store {
    pub fn upsert_cast(
        &self,
        speaker: &str,
        voice_ref: &str,
        kind: &str,
        first_chapter: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cast (speaker, voice_ref, kind, first_chapter) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(speaker) DO UPDATE SET voice_ref = excluded.voice_ref",
            params![speaker, voice_ref, kind, first_chapter],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_lore(
        &self,
        name: &str,
        kind: &str,
        keywords: &str,
        body_md: &str,
        priority: i64,
        always_on: bool,
        updated_chapter: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lore (name, kind, keywords, body_md, priority, always_on, updated_chapter)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(name) DO UPDATE SET
                 kind = excluded.kind,
                 keywords = excluded.keywords,
                 body_md = excluded.body_md,
                 priority = excluded.priority,
                 always_on = excluded.always_on,
                 updated_chapter = excluded.updated_chapter",
            params![
                name,
                kind,
                keywords,
                body_md,
                priority,
                always_on as i64,
                updated_chapter
            ],
        )?;
        Ok(())
    }

    /// Cast speakers ∪ *applied* ledger subjects ∪ lore entries of kind `character`.
    ///
    /// The `applied = 1` filter matters: a delta for a misspelled subject is stored
    /// with `applied = 0` for audit, and without the filter that typo would enter
    /// the known set and be silently accepted on the next attempt — the gate would
    /// teach itself the ghost it just rejected.
    pub fn known_subjects(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT speaker FROM cast
             UNION SELECT subject FROM ledger WHERE applied = 1
             UNION SELECT name FROM lore WHERE kind = 'character'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<BTreeSet<_>>>()?)
    }

    /// Load every ledger row, plus anomalies for rows that could not be decoded.
    ///
    /// Rows are read as raw tuples first and decoded in plain Rust, so a bad `op`
    /// can be skipped-with-a-note rather than coerced or blowing up the query.
    fn entries(&self) -> Result<(Vec<LedgerEntry>, Vec<String>)> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, chapter, subject, field, op, value_num, value_txt, applied
             FROM ledger ORDER BY seq",
        )?;
        let raw = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut entries = Vec::with_capacity(raw.len());
        let mut anomalies = Vec::new();
        for (seq, chapter, subject, field, op, value_num, value_txt, applied) in raw {
            let Some(decoded) = op_from_str(&op) else {
                anomalies.push(format!(
                    "seq {seq} {subject}.{field}: unrecognized op {op:?} — row skipped"
                ));
                continue;
            };
            entries.push(LedgerEntry {
                seq: seq as u64,
                chapter: chapter as u32,
                subject,
                field,
                op: decoded,
                value_num,
                value_txt,
                applied: applied != 0,
            });
        }
        Ok((entries, anomalies))
    }

    /// The derived state snapshot. Decode anomalies are merged into the fold's own
    /// `anomalies` channel, so an undecodable row surfaces to the operator instead
    /// of vanishing.
    pub fn snapshot(&self) -> Result<StateSnapshot> {
        let (entries, mut decode_anomalies) = self.entries()?;
        let mut snap = fold(&entries);
        snap.anomalies.append(&mut decode_anomalies);
        Ok(snap)
    }

    fn next_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger", [], |r| {
                r.get(0)
            })?)
    }

    /// Validate a proposed delta and record it either way.
    ///
    /// The outer `Result` is I/O failure; the inner one is the gate's verdict. A
    /// rejection is **not** an error — it is a stored, auditable outcome.
    pub fn append_delta(
        &self,
        chapter: u32,
        d: &Delta,
    ) -> Result<core::result::Result<(), Rejection>> {
        let snapshot = self.snapshot()?;
        let known = self.known_subjects()?;
        let verdict = validate_delta(&snapshot, &known, d);

        let (applied, reason) = match &verdict {
            Ok(()) => (1i64, String::new()),
            // `code()` not `{:?}`: payload-carrying variants would otherwise render
            // their contents and shatter the drift histogram into one bucket per
            // distinct payload value. The payload is recoverable from the row's own
            // value_num plus the snapshot.
            Err(r) => (0i64, r.code().to_string()),
        };

        let seq = self.next_seq()?;

        self.conn.execute(
            "INSERT INTO ledger
                (chapter, seq, subject, field, op, value_num, value_txt, reason, applied, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                chapter,
                seq,
                d.subject,
                d.field,
                op_str(d.op),
                d.value_num,
                d.value_txt,
                reason,
                applied,
                now_ms(),
            ],
        )?;

        Ok(verdict)
    }

    /// Deltas the gate rejected. Excludes rows deactivated by a rewind — a rewind is
    /// deliberate, and counting it as a rejection would inflate the drift signal
    /// every time you regenerate a stretch of story.
    pub fn rejected_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM ledger WHERE applied = 0 AND reason <> ?1",
            params![REWOUND_REASON],
            |r| r.get(0),
        )?)
    }

    pub fn applied_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM ledger WHERE applied = 1", [], |r| {
                r.get(0)
            })?)
    }

    /// Rejections grouped by `Rejection::code()`, most frequent first. §6.2's
    /// early-warning signal for prompt drift: a rising rate, or a newly dominant
    /// code, means the prompt or the state format is slipping.
    ///
    /// Grouping is clean because `append_delta` stores the payload-free code, so no
    /// caller needs to re-aggregate by string surgery.
    pub fn rejection_reasons(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT reason, COUNT(*) AS n FROM ledger
             WHERE applied = 0 AND reason <> ?1
             GROUP BY reason ORDER BY n DESC, reason",
        )?;
        let rows = stmt.query_map(params![REWOUND_REASON], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Deactivate every applied entry after `through_chapter`. Returns how many
    /// rows changed, so a second call on an already-rewound ledger returns 0.
    pub fn rewind(&self, through_chapter: u32) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE ledger SET applied = 0, reason = ?2
             WHERE chapter > ?1 AND applied = 1",
            params![through_chapter, REWOUND_REASON],
        )?)
    }

    /// What [`Store::rewind`] *would* change: `(rows affected, distinct chapters)`.
    /// Uses the same predicate as `rewind`, so a confirmation prompt cannot describe
    /// something different from what happens.
    pub fn rewind_preview(&self, through_chapter: u32) -> Result<(usize, Vec<u32>)> {
        let rows: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ledger WHERE chapter > ?1 AND applied = 1",
            params![through_chapter],
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT chapter FROM ledger
             WHERE chapter > ?1 AND applied = 1 ORDER BY chapter",
        )?;
        let chapters = stmt
            .query_map(params![through_chapter], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|c| c as u32)
            .collect();
        Ok((rows as usize, chapters))
    }

    pub fn cast(&self) -> Result<Vec<CastRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT speaker, voice_ref, kind, first_chapter FROM cast
             ORDER BY first_chapter, speaker",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CastRow {
                speaker: r.get(0)?,
                voice_ref: r.get(1)?,
                kind: r.get(2)?,
                first_chapter: r.get::<_, i64>(3)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Queue a director note. Returns its rowid.
    pub fn insert_note(&self, body: &str, source: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO notes (body, source, created_at) VALUES (?1, ?2, ?3)",
            params![body, source, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Notes not yet folded into a chapter (§6.4). The engine drains these at a
    /// chapter boundary; it never waits for one.
    pub fn pending_notes(&self) -> Result<Vec<NoteRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, body, source, created_at, consumed_chapter FROM notes
             WHERE consumed_chapter IS NULL ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(NoteRow {
                id: r.get(0)?,
                body: r.get(1)?,
                source: r.get(2)?,
                created_at: r.get(3)?,
                consumed_chapter: r.get::<_, Option<i64>>(4)?.map(|c| c as u32),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Stamp **every** pending note as consumed by `chapter`.
    ///
    /// The argument is the stamp, not a filter — this drains the whole pending queue
    /// regardless of which chapter the notes were written during. That is what §6.4
    /// wants (notes are consumed at the next chapter boundary, whenever they arrived),
    /// but the signature reads like it selects, so: it does not.
    pub fn mark_notes_consumed(&self, chapter: u32) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE notes SET consumed_chapter = ?1 WHERE consumed_chapter IS NULL",
            params![chapter],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num_delta(field: &str, op: Op, n: i64) -> Delta {
        Delta {
            subject: "Kaelen".into(),
            field: field.into(),
            op,
            value_num: Some(n),
            value_txt: None,
        }
    }

    #[test]
    fn op_from_str_is_strict() {
        assert_eq!(op_from_str("set"), Some(Op::Set));
        assert_eq!(op_from_str("add"), Some(Op::Add));
        assert_eq!(op_from_str("sub"), Some(Op::Sub));
        assert_eq!(op_from_str("SET"), None);
        assert_eq!(op_from_str(""), None);
        assert_eq!(op_from_str("increment"), None);
    }

    /// A corrupted `op` must be skipped and reported, never coerced to `Set`.
    /// Coercion is the dangerous default: it would turn "+5 gold" into "gold = 5"
    /// absolutely, and report nothing. Lives here rather than in `tests/` because
    /// it needs crate-private `conn` to simulate the corruption.
    #[test]
    fn unrecognized_op_is_skipped_and_reported_not_coerced() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_cast("Kaelen", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1)
            .unwrap();
        store
            .append_delta(1, &num_delta("gold", Op::Set, 10))
            .unwrap()
            .unwrap();
        store
            .append_delta(1, &num_delta("gold", Op::Add, 5))
            .unwrap()
            .unwrap();
        assert_eq!(store.snapshot().unwrap().num("Kaelen", "gold"), Some(15));

        // Simulate a hand-edit or a bad migration corrupting the op column.
        store
            .conn
            .execute("UPDATE ledger SET op = 'increment' WHERE op = 'add'", [])
            .unwrap();

        let snap = store.snapshot().unwrap();
        // Skipped: gold stays at the Set value. Coercion to Set would give 5.
        assert_eq!(snap.num("Kaelen", "gold"), Some(10));
        assert_eq!(snap.anomalies.len(), 1);
        assert!(snap.anomalies[0].contains("unrecognized op"));
        assert!(snap.anomalies[0].contains("increment"));
    }
}
