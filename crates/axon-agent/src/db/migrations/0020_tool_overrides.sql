-- Persists ToolsPage Enable/Disable toggles across restarts. Without this,
-- ToolRegistry::set_enabled only mutated the in-memory registry, so any
-- toggle (including opting a gated write tool into agent access) was lost on
-- the next boot. Applied on top of the source-registration default and the
-- agent-gate default (see ToolRegistry::apply_agent_gate_defaults) at
-- startup, so a persisted row always wins.
CREATE TABLE IF NOT EXISTS tool_overrides (
    name       TEXT PRIMARY KEY,
    enabled    INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
