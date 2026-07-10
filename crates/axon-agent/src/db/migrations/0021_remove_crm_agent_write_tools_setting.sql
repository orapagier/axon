-- CRM write-tool agent access is now gated per-tool via the ToolsPage
-- Enable/Disable toggle (tool_overrides), same as social/messaging write
-- tools, instead of this single global switch. seed.sql no longer inserts
-- this row; existing databases still have it from earlier seeding, so remove
-- it explicitly here.
DELETE FROM settings WHERE key = 'crm.agent_write_tools';
