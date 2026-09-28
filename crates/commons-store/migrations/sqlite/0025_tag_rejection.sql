-- T-P6-004b step 4 (SQLite mirror). The schema below is byte-identical to
-- postgres/0025_tag_rejection.sql apart from this header, and that was checked
-- by diffing the two with comments stripped -- the comparison migration_parity
-- cannot make for column TYPES. T-P6-004b step 4. A rejection is a record, not a deletion.
--
-- WHY THIS TABLE EXISTS
-- --------------------
-- The ML tag path already has its honesty mechanism: `propose_ml_tag` writes
-- the `ml:` namespace, so a model's opinion can never be stored as fact, and
-- `tagger_queue` is the list of proposals awaiting a decision. None of that
-- needs a table.
--
-- What it does not have is the other half of the decision. `remove_tag` deletes
-- the `object_tag` row, which is correct for "this tag no longer applies" and
-- catastrophically wrong for "no": it leaves nothing behind, so the next
-- transcription run proposes the same topic again, the user dismisses it again,
-- and a library that re-asks what you already told it is a library you stop
-- opening. The dismissal is the information.
--
-- So rejection is recorded rather than performed, and the recording is scoped
-- to (object, source, value) rather than to the tag: a tag can be wrong for one
-- interview and right for the next, and a rejection keyed on the tag would
-- suppress it everywhere.
--
-- This is the same shape as `interview_quote`, one decision earlier in the
-- pipeline: a derived artefact keyed on the OBJECT outlives the run that
-- produced it. A rejection keyed on the transcript would be discarded by the
-- next re-transcription, which is precisely the run most likely to re-propose.
CREATE TABLE tag_rejection (
    id          TEXT PRIMARY KEY,
    -- CASCADE for the same reason as every other object-scoped table: a
    -- rejection about media that is gone is a question nobody will answer.
    object_id   TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
    -- What produced the proposal -- 'ml:tagger', a plugin, a peer. NOT the tag:
    -- see the header.
    source      TEXT NOT NULL,
    -- The proposed value, so a rejection of "climate policy" does not also
    -- suppress "carbon tax" from the same run. JSON, because a proposal's
    -- value is always JSON and a bare string containing a quote would
    -- otherwise be unreadable back.
    value_json  TEXT NOT NULL,
    -- Who said no, and when. A rejection with no author cannot be
    -- distinguished from a system rule, and the two deserve different
    -- treatment in a UI.
    rejected_by TEXT,
    created_at  TEXT NOT NULL,
    -- An empty value_json would match every other empty one and suppress
    -- unrelated proposals. INLINE rather than a trailing ALTER TABLE:
    -- sqlx's bundled SQLite rejects `ALTER TABLE ... ADD CONSTRAINT` with
    -- `near "CONSTRAINT": syntax error`, so a CHECK added after the fact is
    -- a syntax error on one engine and valid on the other. The system
    -- sqlite3 binary (3.53.4) accepts it, which is why this is worth
    -- writing down -- every manual check of this file passed and only the
    -- suite found it. See tests/alt_probe.rs.
    CONSTRAINT tag_rejection_value_present CHECK (length(trim(value_json)) > 2)
);

-- The lookup a re-proposal does: "has this exact proposal already been turned
-- down for this object?" The unique index is what makes the insert idempotent,
-- so a run that races itself does not accumulate duplicates.
CREATE UNIQUE INDEX tag_rejection_uniq_idx
    ON tag_rejection (object_id, source, value_json);
CREATE INDEX tag_rejection_object_idx ON tag_rejection (object_id, created_at);

