-- long_term_fts is an external-content FTS5 index (content=long_term), so it
-- only stays in sync through explicit triggers. The base schema shipped INSERT
-- and DELETE triggers on the assumption that long_term rows are never updated
-- in place.
--
-- Write-side dedup broke that assumption: a repeat memory from the same source
-- now refreshes the existing row rather than inserting a near-identical one.
-- Without this trigger the FTS index would keep serving the row's ORIGINAL
-- text, and since FTS selects the candidate set for every recall, a refreshed
-- memory would be retrievable only by its outdated wording.
--
-- External-content FTS5 requires deleting the old term entries (which needs the
-- pre-update content) before indexing the new ones.
CREATE TRIGGER IF NOT EXISTS long_term_fts_update AFTER UPDATE ON long_term BEGIN
    INSERT INTO long_term_fts(long_term_fts, rowid, content) VALUES('delete', old.id, old.content);
    INSERT INTO long_term_fts(rowid, content) VALUES (new.id, new.content);
END;
