-- Tracks WHY a model is currently disabled, so the dashboard can distinguish a
-- deliberate manual toggle from an automatic park by the Homeostasis health
-- check. NULL = enabled, or disabled before this column existed (unknown
-- reason). 'manual' = disabled via the dashboard toggle/bulk action (or a
-- Homeostasis Update node explicitly setting enabled=false). Any other value
-- is one of the router's health-check failure categories (payment_required,
-- server_error, timeout, unreachable) set by Homeostasis auto-disable.
ALTER TABLE models ADD COLUMN disabled_reason TEXT;
