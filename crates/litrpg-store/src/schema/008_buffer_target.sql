-- 008: the buffer target becomes store state, with the reason it changed.
--
-- It lived in `litrpg.toml` and was read **once**, into an owned `EngineConfig`, at
-- engine startup. The daemon read the same file into its own copy at *its* startup. Two
-- processes therefore held two independent snapshots of one value, and nothing kept them
-- honest: a bump reached the engine only via a restart, and `/api/progress` would happily
-- report a target the engine was not using. That is the same shape as the `sherpa`/`azure`
-- divergence recorded on `EngineConfig::registered_backends` — an instrument that lies in
-- exactly the case it exists for.
--
-- `consumed_through` (003) is already live for both processes because it is a store row
-- read every cycle. This does the same for the other half of the throttle, so
-- `buffer_depth < buffer_target` compares two values that are both current.
--
-- Append-only rather than a mutable column, for two reasons:
--
--   * The project's own discipline is that a bump carries a justification. In
--     `litrpg.toml` that trail was prose in comments (6 -> 7 -> 8 -> 9, each with a
--     written reason). Comments cannot be read back by a program, do not survive a
--     `git checkout`, and cannot record *who* asked. A row per change keeps the trail and
--     makes it queryable.
--   * "State is a fold, not a table" is how the ledger already works. The effective
--     target is the newest row; history is never rewritten.
--
-- Deliberately NOT a column on `story`, following the reasoning recorded in 005: `story`
-- is narrative state, and a caller reading the story row should not be handed operational
-- knobs. It is also not `consumed_through` and must never be confused with it — "I want
-- another chapter" and "I finished chapter 3" are different claims, and only one of them
-- is true at any given moment.
--
-- An empty table means "nobody has ever set a target", which is distinct and useful: the
-- reader falls back to the config value, so `litrpg.toml` keeps working as the seed for a
-- fresh install and no migration has to invent a number.
CREATE TABLE buffer_target_changes (
    id            INTEGER PRIMARY KEY,
    -- The absolute target this change established. Relative bumps are resolved against
    -- the effective value *before* the write, so this column is always the answer and
    -- never an increment to be re-applied.
    target        INTEGER NOT NULL,
    -- Who asked: `cli`, `voice`, `watch`, `candela`. Same vocabulary as `notes.source`,
    -- and for the same reason — a dictated change carries STT risk a typed one does not.
    source        TEXT    NOT NULL,
    -- Why. Free text, required and non-empty: a bump without a reason is precisely what
    -- the comment trail existed to prevent.
    justification TEXT    NOT NULL,
    -- Unix ms.
    at            INTEGER NOT NULL
);

-- The only read that matters is "the newest row", on every engine cycle.
CREATE INDEX idx_buffer_target_changes_id_desc ON buffer_target_changes (id DESC);
