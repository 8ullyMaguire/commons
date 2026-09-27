-- Migration: 0020 playback state
--
-- §11.1 (player, C64) and §12.1.1 (streaming surface). Spec:
-- docs/spec/t-p6-001-player.md. Module: crates/commons-store/src/playback.rs.
--
-- # Why this is a table and not columns on `object`
--
-- `object` is the library's central table. Scans, dedup passes, tag writes and
-- every metadata update go through it. A player's write path is fired every
-- time somebody lets a video sit for ten seconds, so putting it there means the
-- most frequent write in the application contends with the heaviest. A
-- separate table with its own row also makes "which objects have a resume
-- point" a query instead of a full scan with a NULL filter.
--
-- # Why the key is `object_id` alone
--
-- The library is single-user today. Multi-user is T-P9-001, and §12.1's role
-- table is that phase's work -- this table gains `user_id` and a composite
-- primary key then, which is a small-table migration. Recorded here so the next
-- reader knows the single column is a decision with a date, not an oversight.
--
-- # What is NOT NULL and why
--
--   * `position_ms` defaults to 0 rather than being NULL. "Never played" and
--     "played and seeked to the start" resume identically, so separating them
--     would buy a nullable column that every reader then has to COALESCE. What
--     actually changes the UI is `completed`, which is a separate column.
--   * `completed` is 0 rather than NULL: it is a fact about the object, and a
--     fact with three states (unknown / not completed / completed) is a state
--     machine pretending to be a boolean.
--
-- The loop CHECK is the load-bearing constraint. `loop_a_ms < loop_b_ms` is not
-- cosmetic: an inverted or zero-length loop is drawn on the scrubber as armed
-- and then never fires, which is a control bar lying about its own state. The
-- Rust side refuses both before the write (PlaybackError::EmptyLoop /
-- InvertedLoop), so the CHECK is the second line rather than the only one --
-- but it is the line that survives a direct SQL write, a future import, or a
-- bug in a caller that skips validation.

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
