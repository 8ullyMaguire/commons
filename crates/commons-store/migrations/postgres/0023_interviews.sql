-- Migration: 0023 interview transcripts
--
-- §5.8 (interviews: transcription and Q&A search). Spec:
-- docs/spec/t-p6-004-interviews.md. Module: crates/commons-ml/src/asr/.
--
-- # What a transcript is
--
-- A *derived artefact with provenance*, not a fact about the media. Every
-- column that records HOW the words were produced exists so a user can ask
-- "why does this transcript say that" and get an answer that names a model and
-- a digest rather than a shrug.
--
-- # Why words are rows and not a JSON column
--
-- The ticket asks for search. `LIKE` over a JSON blob is not search: it cannot
-- rank, cannot filter by time, and cannot answer "what was said between 4:00
-- and 5:00". The row count is the cost, and the rows are the point.
--
-- # Why `ordinal` is a column
--
-- Word sequence IS data, and a SELECT without an ORDER BY is not ordered. The
-- alternative -- relying on insertion order or on the rowid -- is a promise
-- the engine does not make, and a transcript that reads its words in a
-- different order after a VACUUM is a transcript nobody can trust.

CREATE TABLE interview_transcript (
    id            TEXT PRIMARY KEY,
    -- REFERENCES object(id) ON DELETE CASCADE, matching every other table that
    -- hangs off an object (`file`, `segment`, and 0021's subtitle track). Not
    -- decoration: a transcript whose object has been deleted is a transcript
    -- that claims somebody said something, about nothing, with no way for a
    -- user to tell it from a real one.
    --
    -- UNIQUE, not merely indexed: one transcript per object is the invariant
    -- the whole ticket rests on. A second transcript for the same object is a
    -- re-transcription after a model update, and that REPLACES the row rather
    -- than joining it.
    object_id     TEXT NOT NULL UNIQUE REFERENCES object(id) ON DELETE CASCADE,
    -- The engine, as in `AsrEngine::name()`. Recorded so a transcript can be
    -- reproduced and so a future engine can be told "this was not me".
    engine        TEXT NOT NULL,
    -- The model identifier AND its digest. Both, because they answer different
    -- questions: `model_id` is "which model", and the digest is "which bytes".
    -- `ModelError` verifies the digest at LOAD time -- is this file what the
    -- manifest says -- while the column answers "which model produced THIS
    -- transcript", which is the question a user asks when every transcript in
    -- the library changed overnight.
    model_id      TEXT NOT NULL,
    model_sha256  TEXT NOT NULL,
    -- The ffmpeg argument vector, verbatim. A transcript is only reproducible
    -- if the audio was produced the same way, and "we used ffmpeg" is not a
    -- reproduction. See commons-ml/src/asr/audio.rs::extract_args.
    audio_command TEXT NOT NULL,
    -- Always 16000. Stored rather than assumed because an engine that wants
    -- something else must be able to see what it was actually given.
    sample_rate   INTEGER NOT NULL,
    language      TEXT,
    -- BIGINT, not INTEGER, and the mirror file agrees because it is GENERATED.
    -- The Rust side decodes these as `i64`, and a Postgres `INTEGER` read as
    -- `i64` is a *read-time* type error -- not a write-time one, not a
    -- migration-time one, and not on SQLite at any point, since SQLite widens
    -- INTEGER to 64 bits silently. `migration_parity` compares column NAMES,
    -- so it reports green over a schema that only works on SQLite. This is the
    -- third time this repository has hit it; see 0021_subtitles.sql and
    -- 0022_share_grants.sql (`access_count`).
    word_count    BIGINT NOT NULL DEFAULT 0,
    duration_ms   BIGINT NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    CONSTRAINT interview_sample_rate_is_asr CHECK (sample_rate = 16000),
    -- A word cannot end before it starts, so a transcript cannot either.
    CONSTRAINT interview_duration_sane CHECK (duration_ms >= 0)
);

-- One row per recognised word.
--
-- `confidence` and `speaker` are BOTH nullable and the nullability is the
-- point: "this engine does not score words" and "this engine scored every word
-- 0.5" are different facts, and averaging over both is a lie about the audio.
CREATE TABLE interview_word (
    id            TEXT PRIMARY KEY,
    -- ON DELETE CASCADE so a deleted object takes its words with it. The
    -- transcript is one row per object; orphaning its words would leave a
    -- library whose search returns hits for media that is gone.
    transcript_id TEXT NOT NULL
        REFERENCES interview_transcript(id) ON DELETE CASCADE,
    -- 0-based, dense, and unique per transcript. The uniqueness is enforced in
    -- application code rather than by a database UNIQUE because a bulk insert
    -- with a UNIQUE (transcript_id, ordinal) has to handle a re-run by deleting
    -- first, and the delete is the risky statement -- see the header on
    -- interview_transcript's UNIQUE object_id: a re-transcription REPLACES, and
    -- the replacement path is a transaction in the store module, not a
    -- constraint-driven upsert.
    ordinal       INTEGER NOT NULL,
    -- As recognised: no case folding, no punctuation repair. Normalisation
    -- happens in a derived search column instead, because a transcript that has
    -- been silently corrected is a transcript nobody can audit against audio.
    text          TEXT NOT NULL,
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    -- DOUBLE PRECISION, not REAL. `REAL` is FLOAT4 on Postgres and FLOAT8 on
    -- SQLite, so a Rust `f64` decodes on one engine and is refused by the other
    -- (`Rust type f64 ... not compatible with SQL type FLOAT4`) -- and the
    -- migration-parity test compares column NAMES, so it reports green over a
    -- schema that only works on SQLite. This is the third occurrence of this
    -- in this repository; see 0021_subtitles.sql.
    confidence    DOUBLE PRECISION,
    -- The engine's own speaker label ('SPEAKER_00'), scoped to this
    -- transcript. NOT a person id: joining it to a PersonCluster is a
    -- PROPOSAL, and a type that claimed otherwise would make the uncertainty
    -- inexpressible.
    speaker       TEXT,
    CONSTRAINT interview_word_interval CHECK (end_ms >= start_ms),
    CONSTRAINT interview_word_start_sane CHECK (start_ms >= 0)
);

-- The window an engine was given, so a failed window is a visible GAP rather
-- than a silently short transcript. Without this a transcript that lost 20
-- minutes to errors is indistinguishable from one of a 20-minute interview.
CREATE TABLE interview_window (
    transcript_id TEXT NOT NULL
        REFERENCES interview_transcript(id) ON DELETE CASCADE,
    window_index  INTEGER NOT NULL,
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    -- 1 when the window produced words. 0 with `failure` when it did not.
    ok            INTEGER NOT NULL,
    failure       TEXT,
    CONSTRAINT interview_window_failure_has_reason CHECK (ok = 1 OR failure IS NOT NULL)
);

-- The speakers the engine found in one transcript, and the cluster each is
-- PROPOSED to join.
--
-- `cluster_id` is NULL for every speaker until a user accepts the proposal, and
-- it is nullable rather than pointing at a placeholder cluster precisely so
-- "unlinked" and "linked to nothing" are not the same state.
CREATE TABLE interview_speaker (
    transcript_id TEXT NOT NULL
        REFERENCES interview_transcript(id) ON DELETE CASCADE,
    speaker_key   TEXT NOT NULL,
    label         TEXT,
    -- NULL = not yet proposed. A non-NULL value is a human's decision, never
    -- the diariser's.
    cluster_id    TEXT,
    word_count    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (transcript_id, speaker_key)
);

-- The search path: words for one transcript in time order.
--
-- (transcript_id, start_ms) rather than just (transcript_id), because the
-- commonest read is a RANGE -- "what was said between 4:00 and 5:00" -- and an
-- index on the leading column alone makes that a scan of the whole transcript.
CREATE INDEX interview_word_time ON interview_word(transcript_id, start_ms);

-- Free-text search over a transcript.
--
-- Deliberately a LIKE pre-filter and nothing more: there is no FTS module
-- guarantee across the two engines in this schema, and a search that only works
-- on Postgres is not a search. Ranking happens in the application.
CREATE INDEX interview_word_text ON interview_word(text);

-- The lookup a scan makes: "does this object already have a transcript?"
-- Served by the UNIQUE on object_id, so this is the constraint doing the work.
CREATE INDEX interview_transcript_updated ON interview_transcript(updated_at);
