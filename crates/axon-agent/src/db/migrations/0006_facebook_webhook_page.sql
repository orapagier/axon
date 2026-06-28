-- Per-account routing for inbound Facebook webhook events.
-- Each incoming event is stamped with the Page ID it belongs to (the webhook
-- `entry.id`), so a Facebook Stimulus trigger bound to a specific Page
-- credential only fires for that Page's events.
ALTER TABLE webhook_events ADD COLUMN page_id TEXT;
CREATE INDEX IF NOT EXISTS idx_wh_page ON webhook_events(page_id, source, read, created_at);
