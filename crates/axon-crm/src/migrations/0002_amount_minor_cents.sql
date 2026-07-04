-- Money moves from REAL dollars to INTEGER minor units (cents) to stop float
-- drift in sums. Rebuild the table (SQLite can't drop a column with a CHECK)
-- and backfill amount_minor = ROUND(amount * 100). No table references deals,
-- so dropping the old one is FK-safe.

CREATE TABLE deals_new (
    id             TEXT PRIMARY KEY NOT NULL,
    title          TEXT NOT NULL,
    amount_minor   INTEGER NOT NULL DEFAULT 0 CHECK(amount_minor >= 0),
    currency       TEXT NOT NULL DEFAULT 'USD',
    stage          TEXT NOT NULL CHECK(stage IN ('Prospecting', 'Qualified', 'Proposal', 'Negotiation', 'Won', 'Lost')),
    probability    INTEGER CHECK(probability IS NULL OR (probability >= 0 AND probability <= 100)),
    contact_id     TEXT NOT NULL REFERENCES leads(id) ON DELETE RESTRICT,
    org_id         TEXT REFERENCES orgs(id) ON DELETE SET NULL,
    expected_close TEXT,
    tags           TEXT NOT NULL DEFAULT '[]',
    notes          TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    deleted_at     TEXT
);

INSERT INTO deals_new
    (id, title, amount_minor, currency, stage, probability, contact_id, org_id,
     expected_close, tags, notes, created_at, updated_at, deleted_at)
SELECT id, title, CAST(ROUND(amount * 100) AS INTEGER), currency, stage, probability,
       contact_id, org_id, expected_close, tags, notes, created_at, updated_at, deleted_at
FROM deals;

DROP TABLE deals;
ALTER TABLE deals_new RENAME TO deals;

CREATE INDEX IF NOT EXISTS idx_deals_stage ON deals(stage);
CREATE INDEX IF NOT EXISTS idx_deals_contact_id ON deals(contact_id);
CREATE INDEX IF NOT EXISTS idx_deals_org_id ON deals(org_id);
CREATE INDEX IF NOT EXISTS idx_deals_expected_close ON deals(expected_close);
CREATE INDEX IF NOT EXISTS idx_deals_updated_at ON deals(updated_at);
CREATE INDEX IF NOT EXISTS idx_deals_deleted_at ON deals(deleted_at);
