-- T-P6-004b step 2 (SQLite mirror).
--
-- The schema below is byte-identical to postgres/0024_interview_quotes.sql
-- apart from this header, and that was checked by diffing the two files with
-- comments stripped. It is worth doing by hand because `migration_parity`
-- compares column NAMES and cannot make the comparison for column TYPES --
-- which is the check that has let three FLOAT4/FLOAT8 bugs through this
-- repository.
--
-- Quotes: a span of an interview worth resurfacing.
--
-- WHY A NEW TABLE AND NOT A field_proposal
-- -----------------------------------------
-- A quote is not a proposed correction, and the difference is who may act on
-- it. A correction competes with the engine's word and a person accepts or
-- rejects it. A quote has no competing value -- the engine never said the
-- interview was worth quoting -- so the proposal machinery would give it a
-- subject, a field and a value that nothing is being compared against, and
-- accepting it would mean writing a row that says nothing.
--
-- object_id, NOT transcript_id
-- -----------------------------
-- The load-bearing decision in this file, and the reason it is worth the
-- extra table. A re-transcription REPLACES `interview_word` (see
-- interview_transcript's UNIQUE object_id, and replace_transcript, which
-- deletes words keyed on both the incoming id and the object). A quote cut
-- from those words must NOT be deleted by that: the words may change under a
-- new model while the thing a person found worth quoting has not. So a quote
-- hangs off the OBJECT.
--
-- The cost is that a quote's text is a snapshot and can drift from the words
-- it was cut from. That is the right way round: a quote that silently
-- re-rendered itself from a new transcript is a quote the person did not
-- choose, and the drift is visible (the quote says one thing, the transcript
-- another) where an auto-updated quote is not.
--
-- weight is DOUBLE PRECISION, not REAL
-- -----------------------------------
-- `REAL` is FLOAT4 on Postgres and FLOAT8 on SQLite, so a Rust f64 decodes on
-- one engine and is refused by the other -- and `migration_parity` compares
-- column NAMES, so it reports green over a schema that only works on SQLite.
-- This is the third occurrence of this in this repository; see
-- 0021_subtitles.sql and interview_transcript.word_count. Note that
-- `tag.importance` and `vote.weight` in 0001_core.sql ARE `REAL`: those are
-- hand-set or reputation-derived and are fine at FLOAT4. Do not "harmonise"
-- this column to match them -- a computed weight should not be truncated to
-- float4 to agree with an unrelated column on an unrelated table.
CREATE TABLE interview_quote (
    id            TEXT PRIMARY KEY,
    -- Same shape as every other object-scoped table, and for the same reason
    -- interview_transcript carries it: an object deleted must not leave a
    -- quote behind claiming someone said something, about nothing.
    object_id     TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
    -- The span, in absolute media time. A zero-length quote exists, renders,
    -- and means nothing, so the CHECK is in the schema rather than only in the
    -- store: anything that writes here gets the constraint.
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    -- Snapshot text, trimmed. NOT derived from interview_word at read time --
    -- see the header on object_id for why that is the correct direction.
    text          TEXT NOT NULL,
    weight        DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    -- WHO decided this matters, and therefore what scale `weight` is on. A
    -- human marks a quote 1-5; a model emits a salience in 0.0-1.0. Storing
    -- both in one ranking without this column is a silent failure: nothing
    -- errors, and the interface shows the best model output below the worst
    -- human one. See spec t-p6-004b §4.2 and comparable_weight() in
    -- commons-store/src/interview.rs, which is the only place that knows the
    -- two are different units.
    weight_source TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    -- One quote per (object, span, text). Without it a re-run of the same
    -- proposal inserts duplicates and the quote list fills with copies, which
    -- looks like a model that is very confident about one sentence.
    UNIQUE (object_id, start_ms, end_ms, text),
    CONSTRAINT interview_quote_span_sane CHECK (end_ms > start_ms),
    CONSTRAINT interview_quote_start_sane CHECK (start_ms >= 0),
    CONSTRAINT interview_quote_text_present CHECK (length(trim(text)) > 0),
    CONSTRAINT interview_quote_weight_source CHECK (
        weight_source IN ('human', 'model')
    )
);

-- Time order is the only order a quote list can be read in.
CREATE INDEX interview_quote_object_idx ON interview_quote (object_id, start_ms);
