-- 0020_playback_state.sql - SQLite mirror (Postgres: 0020_playback_state.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0020_playback_state.sql and re-run that script.
-- T-P0-007's parity test fails if the two files' table sets ever diverge.
--
-- Portable-SQL rules in force (plan section 0.4):
--   * ids are TEXT
--   * timestamps are ISO-8601 UTC TEXT, so comparison and sort need no
--     timezone function and both engines agree
--   * no vector columns; embeddings live in a sidecar ANN file
--   * booleans are INTEGER 0|1 here and BOOLEAN in Postgres; the store crate
--     hides the difference and no query writes a literal
--   * foreign keys need `PRAGMA foreign_keys = ON` per connection, which the
--     store crate sets at open time

CREATE TABLE playback_state (
    object_id    TEXT PRIMARY KEY,
    position_ms  INTEGER NOT NULL DEFAULT 0,
    -- From the probe. NULL when no probe has run for this object, which is the
    -- normal case for a file the scanner has not identified yet. `position_ms`
    -- is then unchecked, because there is nothing to check it against.
    duration_ms  INTEGER,
    -- 0 means "this end is not set", not "the start of the video". The control
    -- bar can therefore draw a marker the user dragged to zero, and
    -- LoopPoints::is_armed is what decides whether the loop fires.
    loop_a_ms    INTEGER,
    loop_b_ms    INTEGER,
    completed    INTEGER NOT NULL DEFAULT 0,
    -- ISO-8601 UTC TEXT per plan §0.4, so comparison and sort need no timezone
    -- function and both engines agree. NOT a millisecond integer: that was the
    -- first draft of the spec, and it is the one thing in it that contradicted
    -- the portable-SQL rules the other 19 migrations follow.
    updated_at   TEXT NOT NULL DEFAULT '',
    CONSTRAINT playback_loop_ordered
        CHECK (loop_a_ms IS NULL OR loop_b_ms IS NULL OR loop_a_ms < loop_b_ms)
);

-- No secondary index. The only query the player makes is by `object_id`, which
-- the primary key covers, and there is no "everything I have finished" query in
-- this ticket. An index for a query nobody runs is a write cost on every resume
-- save, which is the most frequent write in the application.
