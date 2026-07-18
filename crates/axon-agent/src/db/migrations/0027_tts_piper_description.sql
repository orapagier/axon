-- tts.base_url/tts.model were seeded (seed.sql, INSERT OR IGNORE) before the
-- local Piper engine existed, so any install that had already started once
-- keeps its original description forever — seeding never rewrites a row that's
-- already there. Those installs' Settings page never mentions Piper is an
-- option, only Groq/OpenAI/Gemini. Backfill the current wording once; leaves
-- `value` (the user's configured base_url/model) untouched.
UPDATE settings
SET description = 'Speech base URL (Groq: https://api.groq.com/openai/v1, OpenAI: https://api.openai.com/v1, Gemini: https://generativelanguage.googleapis.com/v1beta/openai — Gemini is auto-served via its native speech API; type the literal word "piper" for a free offline local voice — run deploy/setup-piper.sh to install it). Blank disables spoken replies — the dashboard falls back to the browser''s built-in voice.'
WHERE key = 'tts.base_url';

UPDATE settings
SET description = 'Speech-synthesis model. Pick from the dropdown (prefetched from tts.base_url''s /models catalogue) or type any ID (e.g. playai-tts on Groq, gpt-4o-mini-tts on OpenAI, gemini-2.5-flash-preview-tts on Gemini, or an installed voice like en_US-lessac-medium when tts.base_url is "piper").'
WHERE key = 'tts.model';
