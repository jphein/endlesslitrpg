-- 005: a liveness signal for the engine process.
--
-- The CLI cannot observe whether `litrpg-engine` is running. It is a separate binary
-- from `litrpg-daemon`, so probing `bind_addr` proves the *daemon* is up while saying
-- nothing about the engine — and the engine owns the resume path. That left commands
-- like `litrpg render <N>`, whose whole effect is "the engine will pick this up", unable
-- to tell a queued chapter from one queued into a void. The failure mode is silence.
--
-- Deliberately a separate table rather than columns on `story`:
--
--   * `story` is narrative state. This is operational state about a process. A caller
--     reading the story row should not receive a field that changes every 45 seconds.
--   * The engine already writes `arc_outline_md` and `prompt_hash` on that row. A
--     heartbeat there would either bump `story.updated_at` every poll cycle — making it
--     track the heartbeat instead of tracking actual changes, a field that still exists
--     and silently stops meaning anything — or need a write path that skips it, which is
--     a rule someone eventually breaks.
--   * Absent row means "no engine has ever run against this database", which is a
--     distinct and useful answer. A column on a table that `init` populates could not
--     express it.
--
-- `CHECK (id = 1)` makes the singleton a schema property rather than a convention, so
-- the writer can be a single `ON CONFLICT DO UPDATE` upsert with no read and no race.
CREATE TABLE engine_heartbeat (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    -- Unix ms, stamped once per poll cycle. Staleness is only meaningful against the
    -- engine's `poll_interval_secs`, which lives in config, so the store reports the
    -- timestamp and refuses to judge it.
    seen_at  INTEGER NOT NULL,
    pid      INTEGER NOT NULL,
    -- The engine's crate version, so `litrpg status` can report a running binary that
    -- predates the database it is writing to.
    version  TEXT    NOT NULL,
    -- JSON array of the TTS backends the running engine actually registered, e.g.
    -- ["azure","sherpa"]. This is the column that earns the table: a binary built
    -- without `--features sherpa` silently substitutes another backend for every sherpa
    -- `voice_ref`, and nothing at any layer could see it — the cast rows said sherpa, the
    -- renders said azure, and the two never met. Recording the live registry lets
    -- `litrpg cast` compare a cast member's backend against what the engine can serve.
    backends TEXT    NOT NULL
);
