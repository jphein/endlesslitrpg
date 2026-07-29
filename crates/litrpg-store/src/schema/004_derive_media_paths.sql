-- Stop storing what can be derived.
--
-- `chapters.pcm_path` and `chapters.mp3_path` restated an answer the layout already
-- gives: `media_dir` + a zero-padded chapter number + an extension. A duplicate can
-- disagree with reality, and this one did — moving the story into the project folder
-- left both columns pointing at a deleted directory, so `litrpg play 1` reported
-- "recorded as having audio, but its media is unusable" about a file sitting
-- perfectly intact one directory away.
--
-- Three arguments settled it, and the third is the project's own:
--
--   * §8 lists four artifacts. `text_md` and `manifest_json` store *content* with no
--     `md_path` or `json_path` column, so two were stored and two derived with no
--     principled distinction. Deriving all four makes the schema consistent.
--   * The filesystem is already authoritative for existence — `play` stats the file
--     regardless, because `has_audio` is a flag and files get pruned underneath it.
--     A stored path's presence was never trustworthy.
--   * §6.1: "there is no `characters.hp` column" because state is a fold. Storing a
--     derivable value is exactly what this design refuses. `ledger.chapter_id` was
--     removed for the same reason.
--
-- `has_audio` stays. That means "rendered; a manifest and segment rows exist", which
-- is engine state and genuinely not derivable from the filesystem.
--
-- The one real bit the columns carried was per-artifact presence, since §8 prunes
-- `.pcm` while `.mp3` is permanent. But nothing updated the column when pruning
-- happened, so it was a stale bit rather than a buffer-window bit. If that fact is
-- ever wanted in the database it deserves an explicit `pcm_pruned_at`, not inference
-- from a path string.

-- Forensics first. The stale-path bug was diagnosable *because* the old values were
-- on record, so they are preserved here rather than dropped with the columns.
CREATE TABLE _audit_004_paths (
    kind   TEXT NOT NULL,
    number INTEGER,
    value  TEXT NOT NULL
);

INSERT INTO _audit_004_paths (kind, number, value)
    SELECT 'chapters.pcm_path', number, pcm_path FROM chapters WHERE pcm_path IS NOT NULL;
INSERT INTO _audit_004_paths (kind, number, value)
    SELECT 'chapters.mp3_path', number, mp3_path FROM chapters WHERE mp3_path IS NOT NULL;
INSERT INTO _audit_004_paths (kind, number, value)
    SELECT 'story.prompt_path', NULL, prompt_path FROM story WHERE prompt_path <> '';

ALTER TABLE chapters DROP COLUMN pcm_path;
ALTER TABLE chapters DROP COLUMN mp3_path;

-- `story.prompt_path` becomes relative to `story_dir`, so a project folder is
-- portable: copy it elsewhere and the row still resolves. Kept rather than derived
-- because it is the one artifact path an operator could plausibly want to point
-- somewhere else, and keeping a relative string preserves that option for the cost of
-- one join on read.
--
-- `rtrim(p, replace(p, '/', ''))` yields the directory prefix including its trailing
-- slash, so removing it leaves the basename. A value with no '/' is left alone,
-- because `replace(x, '', '')` is a no-op in SQLite.
UPDATE story
   SET prompt_path = replace(prompt_path, rtrim(prompt_path, replace(prompt_path, '/', '')), '')
 WHERE prompt_path LIKE '%/%';
