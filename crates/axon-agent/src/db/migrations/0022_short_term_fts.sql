-- Full-text search over chat message content, mirroring the long_term_fts /
-- observations_fts pattern (see 0001_base_schema.sql). Powers "search chat
-- history" on the Chat page. short_term rows are never edited in place (only
-- inserted/deleted), so — same as long_term_fts — no update trigger is needed.
CREATE VIRTUAL TABLE IF NOT EXISTS short_term_fts
    USING fts5(content, content='short_term', content_rowid='id');

CREATE TRIGGER IF NOT EXISTS short_term_fts_insert AFTER INSERT ON short_term BEGIN
    INSERT INTO short_term_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS short_term_fts_delete AFTER DELETE ON short_term BEGIN
    INSERT INTO short_term_fts(short_term_fts, rowid, content) VALUES('delete', old.id, old.content);
END;

INSERT INTO short_term_fts(rowid, content) SELECT id, content FROM short_term;
