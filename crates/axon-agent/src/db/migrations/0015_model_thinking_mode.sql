-- Harness refactor Phase 3: per-model thinking mode for the Anthropic provider.
-- "adaptive" (Claude 4.6+), "budget" (older Claude models that take a thinking
-- token budget), NULL/"off" = never send a thinking parameter.
ALTER TABLE models ADD COLUMN thinking_mode TEXT;
