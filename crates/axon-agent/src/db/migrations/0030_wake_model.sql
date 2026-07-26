-- The web dashboard's on-device-built "Hey Axon" (or whatever phrase the user
-- chose) wake-word model — built server-side via POST /api/wakeword/build
-- since the browser's rustpotter-worklet (WASM) can only run detection, not
-- training. Single row (id fixed at 1): this app is single-tenant (one master
-- key), so one enrolled model per install, same as everything else here.
CREATE TABLE IF NOT EXISTS wake_model (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    phrase     TEXT NOT NULL,
    model      BLOB NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
