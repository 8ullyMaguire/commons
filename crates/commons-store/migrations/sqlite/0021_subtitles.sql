-- 0021_subtitles.sql - SQLite mirror (Postgres: 0021_subtitles.sql).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0021_subtitles.sql and re-run that script.
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

CREATE TABLE subtitle_documents (
    id                  TEXT PRIMARY KEY,
    object_id           TEXT NOT NULL REFERENCES object(id) ON DELETE CASCADE,
    -- 'embedded' or 'sidecar'. Not nullable and not optional: the two have
    -- different lifecycles (a sidecar can vanish from disk without the object
    -- changing) and different ways of being re-read, and a NULL here would be a
    -- third state nothing wants to handle.
    origin              TEXT NOT NULL,
    -- ffprobe's stream index, for an embedded track. NULL for a sidecar.
    stream_index        BIGINT,
    -- BIGINT for every column the Rust side decodes as i64, and the mirror says
    -- INTEGER for all of them. This is not a typo to be "corrected": on SQLite
    -- INTEGER *is* a 64-bit rowid, and on Postgres INTEGER is INT4. So a
    -- Postgres `INTEGER` column read as `i64` is a type error at READ time --
    -- not at write time, not at migration time, and not on SQLite at any time.
    --
    -- The parity test compares column NAMES between the two trees and so
    -- passes. The store test is what caught this, and only because it
    -- round-trips a row on both engines: see `subtitles_db.rs`. The three
    -- boolean columns below are INT4 in Rust and INT4 here, which is why they
    -- are INTEGER and the rest are not.
    -- The sidecar's path on disk, for a sidecar. NULL when embedded.
    path                TEXT,
    format              TEXT NOT NULL,
    -- BCP-47, normalised. NULL when the file does not declare one.
    language            TEXT,
    is_default          INTEGER NOT NULL DEFAULT 0,
    is_forced           INTEGER NOT NULL DEFAULT 0,
    is_hearing_impaired INTEGER NOT NULL DEFAULT 0,
    -- Of the raw document, so a re-scan can tell "unchanged" from "rewritten".
    sha256              TEXT NOT NULL,
    byte_size           BIGINT NOT NULL,
    extracted_at        TEXT NOT NULL DEFAULT '',
    -- A sidecar HAS a path and an embedded track does NOT. The pair is
    -- redundant, and redundancy here is the point: one of the two being wrong
    -- means a row that claims to be a sidecar with nothing to read, or claims to
    -- be embedded with a path nothing will use. Both are states where the track
    -- list shows an entry that does not work.
    CONSTRAINT subtitle_origin_is_known
        CHECK (origin IN ('embedded', 'sidecar')),
    CONSTRAINT subtitle_sidecar_has_a_path
        CHECK ((origin = 'sidecar') = (path IS NOT NULL)),
    -- An embedded track's index is required. A sidecar has none, and inventing
    -- one would make a sidecar look like stream 0 of the video, which is a
    -- subtitle track that does not exist.
    CONSTRAINT subtitle_embedded_has_a_stream
        CHECK (origin = 'embedded' OR stream_index IS NULL)
);

-- The queries the player and the extractor actually make, and no others.
--
-- "every track for this object, in a stable order" is the track list, and it is
-- on every open of every video -- the single most frequent read in this
-- feature. (object_id, language) is a prefix of that, so the ordering is free
-- and the language grouping the client wants is served by the same index rather
-- than by a sort.
CREATE INDEX subtitle_documents_by_object
    ON subtitle_documents (object_id, language);

-- One row per TRACK, not per digest.
--
-- The obvious key is (object_id, sha256) -- "the same bytes are already stored"
-- -- and it is wrong in a way that deletes a user's subtitle track without an
-- error. A film with an English track and a forced-signs track commonly has
-- *identical* text in the two files (a translation script emits the same
-- cues, and a sign-language track frequently is a copy). Under (object_id,
-- sha256) the second write is treated as "unchanged" and silently skipped, so
-- the player shows one language where the file has two. The store test
-- `two_languages_of_one_object_are_both_kept` is that bug, and it was written
-- before the constraint was, which is the only reason it was found.
--
-- What actually identifies a track is WHERE it is, not what it contains:
-- (object, origin, stream_index, language, format). Re-extracting the same
-- track replaces the row; a track that moved (a remux renumbers stream
-- indices) becomes a new row and the stale one is collected by
-- `delete_documents` on the next re-scan.
--
-- The `stream_index IS NULL` case is the sidecar's, and COALESCE gives it one
-- value instead of a NULL that never equals another NULL in a unique index --
-- on both engines, so two sidecars of one language would otherwise collide
-- with each other.
CREATE UNIQUE INDEX subtitle_documents_by_track
    ON subtitle_documents (
        object_id,
        origin,
        COALESCE(stream_index, -1),
        COALESCE(language, ''),
        format
    );

-- # Why `seq` is stored rather than derived from `start_ms`
--
-- ASS layers its dialogue, so a real file routinely has cues that overlap or
-- run backwards. `ORDER BY start_ms` would renumber such a file, which changes
-- what a diff shows and what a search highlights -- the two things a user would
-- use to check whether a file was parsed correctly. `seq` is the file's own
-- order and nothing sorts it away.
--
-- The bounds are the load-bearing part of the CHECKs. `end_ms > start_ms` is
-- not cosmetic: a browser DISCARDS a WebVTT cue with `end <= start` silently,
-- and a silently discarded cue is indistinguishable from having no subtitles.
-- The Rust side drops such a cue in `to_webvtt` for the same reason, so the
-- constraint is the second line rather than the only one.
CREATE TABLE subtitle_cues (
    id          TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES subtitle_documents(id) ON DELETE CASCADE,
    seq         BIGINT NOT NULL,
    start_ms    BIGINT NOT NULL,
    -- Half-open [start, end), the same convention commons-media::range uses, so
    -- "is a cue showing at t" has one answer in this codebase.
    end_ms      BIGINT NOT NULL,
    -- The text with inline markup STRIPPED, because this is what gets searched
    -- and what gets rendered as WebVTT, where a bare `<` opens a tag that eats
    -- the rest of the line. Stripping is only ever right for those two uses,
    -- which is why the styling is kept beside it rather than discarded.
    text        TEXT NOT NULL,
    -- The source's styling for this cue, as given. Held as a string because
    -- interpreting ASS override tags is a rendering project this ticket
    -- explicitly does not do: the honest position is that we HAVE the styling
    -- and are not interpreting it.
    style_json  TEXT,
    CONSTRAINT subtitle_cue_ordered CHECK (end_ms > start_ms),
    CONSTRAINT subtitle_cue_seq_non_negative CHECK (seq >= 0),
    CONSTRAINT subtitle_cue_not_empty CHECK (length(text) > 0)
);

-- The only query against cues today is "every cue of this document, in file
-- order", which the UNIQUE index below serves as a prefix. The caption-text
-- search of #4985 gets its own index when that ticket lands, and putting a
-- speculative one here would be a write cost on every extraction for a query
-- that does not exist yet.
CREATE UNIQUE INDEX subtitle_cues_by_document_seq
    ON subtitle_cues (document_id, seq);

-- # No index on `text`, deliberately, and the one that will come
--
-- #4985 wants to search caption text. It cannot be served by a btree: the query
-- is a substring or a full-text match, and an index that does not answer it is
-- a write cost on every extraction for nothing. The search ticket adds the
-- engine its own dialect supports (Postgres tsvector, SQLite FTS5) and the
-- SQLite mirror of that will be a table rather than an index -- which is the
-- only structural difference between the two engines in this migration.
