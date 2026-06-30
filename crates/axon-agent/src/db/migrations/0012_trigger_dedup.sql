-- C2: Trigger idempotency / dedup.
--
-- Event-sourced triggers (a GitHub webhook redelivery, a generic webhook retried
-- by its sender) can deliver the SAME event more than once — on a sender retry,
-- an overlapping poll, or right after an agent restart — double-firing a run.
-- Before firing, the receiver INSERT-OR-IGNOREs the event's idempotency key here;
-- a 0-row insert means "already processed" and the fire is skipped.
--
--   source     - 'github' | 'webhook' | … (the trigger family)
--   event_key  - the sender's idempotency token (GitHub X-Github-Delivery), an
--                explicit Idempotency-Key / event_id, or a body hash + time bucket
--                for callers that supply neither. Scoped per workflow so the same
--                event delivered to two workflow URLs still fires both.
--
-- Pruned by maintenance (age-based) so the table can't grow unbounded.
-- NOT used for interactive chat (every Telegram/Discord message is intentionally
-- distinct) — only genuinely event-sourced, retry-prone triggers dedup.
CREATE TABLE IF NOT EXISTS trigger_dedup (
    source     TEXT NOT NULL,
    event_key  TEXT NOT NULL,
    seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (source, event_key)
);

CREATE INDEX IF NOT EXISTS idx_trigger_dedup_seen ON trigger_dedup(seen_at);
