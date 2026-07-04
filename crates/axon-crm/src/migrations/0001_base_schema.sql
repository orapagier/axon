-- Baseline: the schema as it existed before versioned migrations. Every
-- statement is idempotent (IF NOT EXISTS) so databases created by the old
-- ad-hoc setup adopt versioning without changes; deleted_at (previously added
-- by ensure_column) is inlined for fresh databases.

CREATE TABLE IF NOT EXISTS orgs (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    website    TEXT,
    industry   TEXT,
    size       TEXT,
    country    TEXT,
    phone      TEXT,
    email      TEXT,
    tags       TEXT NOT NULL DEFAULT '[]',
    notes      TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
);

CREATE TABLE IF NOT EXISTS leads (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    email      TEXT,
    phone      TEXT,
    company    TEXT,
    org_id     TEXT REFERENCES orgs(id) ON DELETE SET NULL,
    status     TEXT NOT NULL CHECK(status IN ('Open', 'Contacted', 'Qualified', 'Lost')),
    source     TEXT,
    tags       TEXT NOT NULL DEFAULT '[]',
    notes      TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
);

CREATE TABLE IF NOT EXISTS deals (
    id             TEXT PRIMARY KEY NOT NULL,
    title          TEXT NOT NULL,
    amount         REAL NOT NULL DEFAULT 0 CHECK(amount >= 0),
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

CREATE TABLE IF NOT EXISTS activities (
    id          TEXT PRIMARY KEY NOT NULL,
    entity_id   TEXT NOT NULL,
    entity_type TEXT NOT NULL CHECK(entity_type IN ('lead', 'deal', 'org')),
    kind        TEXT NOT NULL CHECK(kind IN ('note', 'call', 'email', 'meeting', 'task', 'other')),
    title       TEXT NOT NULL,
    body        TEXT,
    occurred_at TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    deleted_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_orgs_name ON orgs(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_orgs_industry ON orgs(industry);
CREATE INDEX IF NOT EXISTS idx_orgs_updated_at ON orgs(updated_at);
CREATE INDEX IF NOT EXISTS idx_orgs_deleted_at ON orgs(deleted_at);
CREATE INDEX IF NOT EXISTS idx_leads_status ON leads(status);
CREATE INDEX IF NOT EXISTS idx_leads_email ON leads(email COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_leads_org_id ON leads(org_id);
CREATE INDEX IF NOT EXISTS idx_leads_updated_at ON leads(updated_at);
CREATE INDEX IF NOT EXISTS idx_leads_deleted_at ON leads(deleted_at);
CREATE INDEX IF NOT EXISTS idx_deals_stage ON deals(stage);
CREATE INDEX IF NOT EXISTS idx_deals_contact_id ON deals(contact_id);
CREATE INDEX IF NOT EXISTS idx_deals_org_id ON deals(org_id);
CREATE INDEX IF NOT EXISTS idx_deals_expected_close ON deals(expected_close);
CREATE INDEX IF NOT EXISTS idx_deals_updated_at ON deals(updated_at);
CREATE INDEX IF NOT EXISTS idx_deals_deleted_at ON deals(deleted_at);
CREATE INDEX IF NOT EXISTS idx_activities_entity ON activities(entity_id, entity_type);
CREATE INDEX IF NOT EXISTS idx_activities_kind ON activities(kind);
CREATE INDEX IF NOT EXISTS idx_activities_occurred_at ON activities(occurred_at);
CREATE INDEX IF NOT EXISTS idx_activities_deleted_at ON activities(deleted_at);
