//! The engine's liveness signal — see `schema/005_engine_heartbeat.sql` for why it is a
//! table of its own.
//!
//! The store records what the engine reported and **refuses to judge it**. Whether
//! `seen_at` is stale depends on the engine's `poll_interval_secs`, which lives in config
//! that this crate does not read; a hardcoded threshold here would be a second place
//! deciding what "alive" means, and the two would drift the moment someone changed the
//! poll interval.

use rusqlite::params;

use crate::{Result, Store, now_ms};

/// What the running engine last reported about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHeartbeat {
    /// Unix ms of the last poll cycle. Compare against the engine's `poll_interval_secs`
    /// to decide staleness; the store deliberately does not.
    pub seen_at: i64,
    pub pid: i64,
    /// The engine binary's crate version, which may predate the database it is writing to.
    pub version: String,
    /// The TTS backends the running engine actually registered.
    ///
    /// The point of the heartbeat: a cast member with a `sherpa:` voice cannot be rendered
    /// by an engine that registered only `["azure"]`, and before this there was no way to
    /// notice — the substitution happened silently inside the render and the cast row kept
    /// claiming a backend nobody had.
    pub backends: Vec<String>,
}

impl Store {
    /// Record that the engine is alive. Called once per poll cycle.
    ///
    /// `pid` and `version` are parameters rather than read here on purpose: this crate is
    /// linked into the CLI and the daemon too, so `std::process::id()` and
    /// `CARGO_PKG_VERSION` evaluated *here* would describe whichever process happened to
    /// call, and `CARGO_PKG_VERSION` would be the store's version rather than the
    /// engine's. Making the caller state who it is keeps the row honest and lets tests
    /// stamp a fake process.
    ///
    /// One upsert against a `CHECK (id = 1)` singleton — no read, so two engines racing
    /// produce a last-writer-wins timestamp rather than a duplicate row. That is the right
    /// semantic: the question a caller is asking is "is *an* engine alive", and if two are
    /// running that is a separate problem this row is not trying to solve.
    pub fn stamp_engine_heartbeat(
        &self,
        pid: u32,
        version: &str,
        backends: &[String],
    ) -> Result<()> {
        let backends = serde_json::to_string(backends)?;
        self.conn.execute(
            "INSERT INTO engine_heartbeat (id, seen_at, pid, version, backends)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 seen_at  = excluded.seen_at,
                 pid      = excluded.pid,
                 version  = excluded.version,
                 backends = excluded.backends",
            params![now_ms(), pid, version, backends],
        )?;
        Ok(())
    }

    /// What the engine last reported, or `None` if no engine has ever run against this
    /// database.
    ///
    /// `None` and "stale" are different answers and callers should say different things:
    /// nothing has ever rendered here, versus something rendered and then stopped.
    pub fn engine_heartbeat(&self) -> Result<Option<EngineHeartbeat>> {
        let row: Option<(i64, i64, String, String)> = self
            .conn
            .query_row(
                "SELECT seen_at, pid, version, backends FROM engine_heartbeat WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((seen_at, pid, version, backends)) = row else {
            return Ok(None);
        };
        // Strict: this column is written by exactly one function serialising a
        // `Vec<String>`, so malformed JSON is not a model being creative — it is
        // corruption or a hand edit, and the spec's leniency rule (§5.3) covers input
        // from Ember, not a protocol we wrote for ourselves.
        let backends: Vec<String> = serde_json::from_str(&backends)?;
        Ok(Some(EngineHeartbeat {
            seen_at,
            pid,
            version,
            backends,
        }))
    }
}
