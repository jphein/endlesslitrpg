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

fn op_from_str(s: &str) -> Op {
    match s {
        "add" => Op::Add,
        "sub" => Op::Sub,
        _ => Op::Set,
    }
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

    fn entries(&self) -> Result<Vec<LedgerEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, chapter, subject, field, op, value_num, value_txt, applied
             FROM ledger ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LedgerEntry {
                seq: r.get::<_, i64>(0)? as u64,
                chapter: r.get::<_, i64>(1)? as u32,
                subject: r.get(2)?,
                field: r.get(3)?,
                op: op_from_str(&r.get::<_, String>(4)?),
                value_num: r.get(5)?,
                value_txt: r.get(6)?,
                applied: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn snapshot(&self) -> Result<StateSnapshot> {
        Ok(fold(&self.entries()?))
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
            Err(r) => (0i64, format!("{r:?}")),
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

    pub fn rejected_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM ledger WHERE applied = 0", [], |r| {
                r.get(0)
            })?)
    }

    /// Deactivate every applied entry after `through_chapter`. Returns how many
    /// rows changed, so a second call on an already-rewound ledger returns 0.
    pub fn rewind(&self, through_chapter: u32) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE ledger SET applied = 0, reason = 'rewound'
             WHERE chapter > ?1 AND applied = 1",
            params![through_chapter],
        )?)
    }
}
