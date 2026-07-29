CREATE TABLE story (
    id              INTEGER PRIMARY KEY,
    title           TEXT    NOT NULL,
    protagonist     TEXT    NOT NULL DEFAULT '',
    prompt_path     TEXT    NOT NULL,
    prompt_hash     TEXT    NOT NULL DEFAULT '',
    arc_outline_md  TEXT    NOT NULL DEFAULT '',
    target_words    INTEGER NOT NULL DEFAULT 2000,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE chapters (
    id            INTEGER PRIMARY KEY,
    number        INTEGER NOT NULL UNIQUE,
    title         TEXT    NOT NULL,
    text_md       TEXT    NOT NULL,
    prompt_hash   TEXT    NOT NULL DEFAULT '',
    pcm_path      TEXT,
    mp3_path      TEXT,
    manifest_json TEXT,
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    has_audio     INTEGER NOT NULL DEFAULT 0,
    state_dirty   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE segments (
    id         INTEGER PRIMARY KEY,
    chapter_id INTEGER NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,
    speaker    TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    text       TEXT    NOT NULL,
    voice_ref  TEXT    NOT NULL,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    UNIQUE (chapter_id, idx)
);

CREATE TABLE cast (
    id            INTEGER PRIMARY KEY,
    speaker       TEXT    NOT NULL UNIQUE,
    voice_ref     TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    first_chapter INTEGER NOT NULL
);

CREATE TABLE lore (
    id              INTEGER PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    kind            TEXT    NOT NULL,
    keywords        TEXT    NOT NULL DEFAULT '',
    body_md         TEXT    NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 0,
    always_on       INTEGER NOT NULL DEFAULT 0,
    updated_chapter INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE ledger (
    id         INTEGER PRIMARY KEY,
    chapter_id INTEGER,
    chapter    INTEGER NOT NULL,
    seq        INTEGER NOT NULL UNIQUE,
    subject    TEXT    NOT NULL,
    field      TEXT    NOT NULL,
    op         TEXT    NOT NULL,
    value_num  INTEGER,
    value_txt  TEXT,
    reason     TEXT    NOT NULL DEFAULT '',
    applied    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE INDEX ledger_seq_idx ON ledger (seq);
CREATE INDEX ledger_chapter_idx ON ledger (chapter);

CREATE TABLE summaries (
    id      INTEGER PRIMARY KEY,
    level   INTEGER NOT NULL,
    from_ch INTEGER NOT NULL,
    to_ch   INTEGER NOT NULL,
    body_md TEXT    NOT NULL
);

CREATE TABLE notes (
    id               INTEGER PRIMARY KEY,
    body             TEXT    NOT NULL,
    source           TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    consumed_chapter INTEGER
);
