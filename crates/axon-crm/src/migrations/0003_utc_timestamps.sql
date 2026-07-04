-- Comparable timestamps: expected_close / occurred_at previously accepted any
-- RFC 3339 offset, but views compare them lexicographically against UTC
-- cutoffs. Rewrite existing rows to the fixed UTC format the code now writes
-- ('%Y-%m-%dT%H:%M:%fZ' == 2026-07-04T12:00:00.000Z). SQLite's strftime parses
-- offset suffixes and converts to UTC; unparseable values are left untouched
-- (strftime returns NULL, COALESCE keeps the original).

UPDATE deals
   SET expected_close = COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', expected_close), expected_close)
 WHERE expected_close IS NOT NULL;

UPDATE activities
   SET occurred_at = COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at), occurred_at);
