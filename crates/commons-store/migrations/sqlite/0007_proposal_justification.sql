-- Migration: 0007 proposal_justification
--
-- Mirrors postgres/0007. A bare ADD COLUMN needs no `--:sqlite` block: SQLite
-- has supported the form since 3.35 and the parity test checks the column
-- appears in both.
ALTER TABLE field_proposal ADD COLUMN justification TEXT;
