-- Maps a Telegram message sent BY a workflow back to that workflow.
--
-- When the user "reply to"s such a message, the reply is routed into the
-- originating workflow's telegram trigger node (instead of the main agent),
-- carrying both the reply and the original replied-to message. A reply to any
-- other message (e.g. one the main agent sent) is not present here and falls
-- through to the agent as before.
CREATE TABLE IF NOT EXISTS telegram_reply_routes (
    chat_id     TEXT    NOT NULL,
    message_id  INTEGER NOT NULL,
    workflow_id TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (chat_id, message_id)
);

CREATE INDEX IF NOT EXISTS idx_telegram_reply_routes_created
    ON telegram_reply_routes (created_at);
