//! The buffer target and the trail of why it changed (schema 008).
//!
//! The effective target is the newest row in `buffer_target_changes`, or nothing at all
//! when no one has ever set one — in which case the caller falls back to its configured
//! value. That fallback is why this returns `Option<u32>` rather than inventing a
//! default: the store does not know what `litrpg.toml` says, and guessing would put a
//! number in the throttle that no one chose.

use rusqlite::params;

use crate::{Result, Store, now_ms};

/// One recorded change of the buffer target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferTargetChange {
    pub id: i64,
    /// The absolute target established by this change. A relative bump is resolved
    /// before the write, so this is never an increment.
    pub target: u32,
    pub source: String,
    pub justification: String,
    /// Unix ms.
    pub at: i64,
}

impl Store {
    /// The target in force, or `None` when the table is empty.
    ///
    /// Read on **every** engine cycle, which is the whole point of the table — see the
    /// migration comment. Ordered by `id` rather than `at`: two changes inside the same
    /// millisecond are possible, and `id` is the only total order the table guarantees.
    pub fn buffer_target(&self) -> Result<Option<u32>> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT target FROM buffer_target_changes ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(n.map(|v| v as u32))
    }

    /// Record a new absolute target and return the persisted row.
    ///
    /// Validation of *what* is a sane target belongs to the caller: the store does not
    /// know the configured minimum, and a rule enforced in two layers is a rule that
    /// eventually disagrees with itself. What the store guarantees is that the write is
    /// append-only and stamped with its own clock.
    pub fn record_buffer_target(
        &self,
        target: u32,
        source: &str,
        justification: &str,
    ) -> Result<BufferTargetChange> {
        let at = now_ms();
        self.conn.execute(
            "INSERT INTO buffer_target_changes (target, source, justification, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![target, source, justification, at],
        )?;
        Ok(BufferTargetChange {
            id: self.conn.last_insert_rowid(),
            target,
            source: source.to_string(),
            justification: justification.to_string(),
            at,
        })
    }

    /// The change trail, newest first. The queryable replacement for what used to be
    /// prose in `litrpg.toml`'s comments.
    pub fn buffer_target_history(&self, limit: u32) -> Result<Vec<BufferTargetChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, target, source, justification, at
             FROM buffer_target_changes
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(BufferTargetChange {
                id: r.get(0)?,
                target: r.get::<_, i64>(1)? as u32,
                source: r.get(2)?,
                justification: r.get(3)?,
                at: r.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
