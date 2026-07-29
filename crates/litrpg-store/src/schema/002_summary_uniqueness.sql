-- A summary is identified by its level and the chapter range it covers, so
-- re-extracting a `state_dirty` chapter must replace its summary rather than
-- append a second one. Without this constraint, `put_summary` would have to
-- DELETE-then-INSERT and race itself; with it, ON CONFLICT does the work.
CREATE UNIQUE INDEX summaries_level_range_idx ON summaries (level, from_ch, to_ch);

-- Resume scans for chapters whose text shipped but whose audio did not (§10).
CREATE INDEX chapters_has_audio_idx ON chapters (has_audio);
