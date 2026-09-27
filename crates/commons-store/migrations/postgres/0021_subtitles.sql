-- Migration: 0021 subtitles
--
-- §5.10 (C10) and §11.1. Spec: docs/spec/t-p6-002-subtitles.md.
-- Module: crates/commons-store/src/subtitles.rs.
--
-- # Two tables, and the reason the text is not in one
--
-- `subtitle_documents` is the RECORD: one row per subtitle source, holding the
-- raw bytes' identity, the format, the language and the dispositions. It is
-- what a user means by "the English subtitles", and it is the thing the track
-- list shows.
--
-- `subtitle_cues` is an INDEX over that record: one row per cue, with the text
-- and timings, so that #4985 ("search the text of caption files") is a query
-- rather than a project. Nothing is lost by splitting them, because the
-- document row keeps the raw bytes' SHA and the source path -- so the original
-- ASS styling is still there, and a cue-level style cache can be added later
-- without re-extracting anything.
--
-- The alternative -- cues as blobs of text with the styling inline -- is what
-- makes ASS unfixable later, because the moment the text is a string the
-- original is gone. Measured while writing the spec: ffmpeg's probe gives a
-- subtitle stream's language, its dispositions and `extradata_size`, and NOT a
-- single character of cue text. Extraction is a decode, and the decoded bytes
-- are the thing worth keeping.
--
-- # Why `sha256` and not just a path
--
-- A re-scan must be idempotent, and "the file is still at this path" is not
-- enough to prove that: a converter rewrites a sidecar with identical cues and
-- a different byte count, and a path-only check treats that as unchanged. The
-- hash makes a re-extract a no-op when the bytes really are the same, and a
-- genuine change when they are not. A duplicate document for the same bytes is
-- a bug a user sees as a doubled entry in the track list, so this is the column
-- that prevents it.
--
-- # Why the dispositions are columns and not a bitfield
--
-- `is_default`, `is_forced` and `is_hearing_impaired` come straight from
-- ffprobe's `disposition`, and they are three separate facts with three
-- separate behaviours: `default` is which track the container wants, `forced`
-- is "burn this in if the user has no preference", and `hearing_impaired` is
-- how the container says "these are captions, not subtitles". Collapsing them
-- into one `flags` integer is how #4586's language rulesets turn into a mess,
-- and a bitfield in SQL is a portability problem on top of that. The
-- authoritative source is the container -- it is what the muxer wrote -- so
-- inferring these from a filename would be second-guessing the file.
--
-- # Why `language` is TEXT and nullable
--
-- NULL means "the file did not say", which is a real and common case and is
-- different from the empty string, which means "the file said it has no
-- language". The value stored is a BCP-47 tag, normalised at the edge: ffprobe
-- reports ISO 639-2 three-letter codes (`eng`), and a filter that matches `en`
-- must find `eng`. Normalising in one place is the whole point -- three
-- spellings of one language is a filter that misses two thirds of the tracks.

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
