-- SQLite has no `ALTER TABLE ... ADD CONSTRAINT`, and it does not enforce
-- CHECK constraints added after a table exists either -- a CHECK in SQLite is
-- part of the column definition in the original CREATE TABLE. So the status
-- vocabulary is enforced by a trigger, which is the one mechanism SQLite has for
-- constraining a write to an existing table.
--
-- The alternative -- the twelve-step table rebuild -- was rejected because it
-- rewrites the whole `performer` table to add a constraint that is four values
-- long, and the rebuild has to be redone correctly for every future constraint.
-- A trigger is one statement and is checked by the parity test on the same terms
-- as any other.
--
-- BEFORE rather than AFTER so the write is refused: an AFTER trigger would let
-- the bad value in and then undo it, which is a row-version bump and an index
-- update wasted on every bad write.
CREATE TRIGGER IF NOT EXISTS performer_status_known
BEFORE INSERT ON performer
FOR EACH ROW
WHEN NEW.status IS NOT NULL
 AND NEW.status NOT IN ('active', 'deceased', 'retired', 'inactive')
BEGIN
    SELECT RAISE(ABORT, 'performer.status is not a known status');
END;

CREATE TRIGGER IF NOT EXISTS performer_status_known_update
BEFORE UPDATE OF status ON performer
FOR EACH ROW
WHEN NEW.status IS NOT NULL
 AND NEW.status NOT IN ('active', 'deceased', 'retired', 'inactive')
BEGIN
    SELECT RAISE(ABORT, 'performer.status is not a known status');
END;
