-- One character recorded under two names, resolved at **read** time (#11).
--
-- The live story has `Kaelen Vord` with 18 ledger entries across chapters 1–5 and `Kaelen`
-- with 6 in chapter 3 alone. They are one person: `story.protagonist` is `Kaelen Vord` while
-- the cast row said `Kaelen`, so pass 2 addressed deltas to whichever the context offered.
-- The protagonist's sheet is split in two and chapter 3's stat changes are invisible on it.
--
-- # Why the ledger is not rewritten
--
-- JP chose this explicitly over rewriting history. Append-only is what makes `rewind N` free:
-- deactivate rows past chapter N and the fold is simply correct again (spec §6.1). A migration
-- that rewrote `ledger.subject` would buy a merged sheet at the cost of the property the whole
-- data model is built on — and it could not be undone.
--
-- So the rows stay exactly as written and the *fold* resolves them. `Store::entries` maps each
-- row's subject through this table before folding, `known_subjects` reports canonical names
-- only, and `append_delta` resolves before validating so a delta addressed to an alias is
-- clamped against the canonical subject's real values rather than against nothing.
--
-- # Keys, not raw strings
--
-- `alias_key` and `canonical_key` hold `litrpg_core::speaker::identity_key` output, so an
-- alias matches regardless of case or internal whitespace. Comparing raw strings here would
-- have been rule number eight.
--
-- Rows are inserted by `Store::add_alias`, which computes both keys from the rule. Nothing is
-- seeded by this file: the `Kaelen -> Kaelen Vord` mapping is applied by a Rust step in
-- `Store::migrate` **only when both subjects are actually present in the ledger**, because a
-- fresh database has no Kaelen and must not be told it does.

CREATE TABLE subject_alias (
    -- identity_key of the name as recorded in the ledger.
    alias_key     TEXT PRIMARY KEY,
    -- Display form of the subject it resolves to.
    canonical     TEXT NOT NULL,
    -- identity_key of `canonical`, so resolution never re-derives it.
    canonical_key TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    -- A name cannot be its own alias, and a chain would make resolution order-dependent:
    -- resolution is deliberately one hop.
    CHECK (alias_key <> canonical_key)
);

CREATE INDEX subject_alias_canonical_idx ON subject_alias (canonical_key);
