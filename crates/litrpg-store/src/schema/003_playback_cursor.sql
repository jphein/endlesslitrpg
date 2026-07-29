-- The playback cursor: the highest chapter the listener has finished.
--
-- Without it, "chapters rendered ahead" has no baseline, so the engine either
-- idles forever at buffer_target or runs unbounded under --drain. Neither is
-- "endless".
--
-- Deliberately a plain column on the singleton story row rather than a new table.
-- It commits to nothing about *who* updates it: the CLI sets it today, and the
-- watch or a Candela source can PUT the same field later without a schema change.
ALTER TABLE story ADD COLUMN consumed_through INTEGER NOT NULL DEFAULT 0;
