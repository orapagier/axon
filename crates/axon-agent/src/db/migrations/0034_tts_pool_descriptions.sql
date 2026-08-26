-- The `tts.*` group is no longer the only place spoken replies are configured:
-- any model tagged role='tts' on the Models page now routes through the model
-- router, which rotates to the next key when one is rate-limited or out of
-- quota. These settings become the *fallback* — used when no role='tts' model
-- exists at all, and when the pool is exhausted.
--
-- Descriptions only; no value is touched, so an existing install keeps speaking
-- exactly as it did.
UPDATE settings
SET description = 'Fallback speech base URL, used when no model is tagged role="tts" on the Models page (and when that pool is exhausted). Groq: https://api.groq.com/openai/v1, OpenAI: https://api.openai.com/v1, Gemini: https://generativelanguage.googleapis.com/v1beta/openai — Gemini is auto-served via its native speech API; type the literal word "piper" for a free offline local voice (run deploy/setup-piper.sh to install it), which makes a good last resort behind a paid pool. Blank, with no TTS models configured, disables spoken replies — the dashboard falls back to the browser''s built-in voice.'
WHERE key = 'tts.base_url';

UPDATE settings
SET description = 'Fallback speech-synthesis model (see tts.base_url). Pick from the dropdown (prefetched from tts.base_url''s /models catalogue) or type any ID (e.g. playai-tts on Groq, gpt-4o-mini-tts on OpenAI, gemini-2.5-flash-preview-tts on Gemini, or an installed voice like en_US-lessac-medium when tts.base_url is "piper"). Models tagged role="tts" on the Models page are configured there instead, and take precedence.'
WHERE key = 'tts.model';

UPDATE settings
SET description = 'Voice name, required by most providers (Groq playai-tts: Fritz-PlayAI, Arista-PlayAI, …; OpenAI: alloy, echo, nova, …; Gemini: Kore, Puck, Zephyr, Charon, …). Also the default for any role="tts" model whose own Voice field is left blank — so a pool added on top of an existing setup keeps the voice it already spoke in.'
WHERE key = 'tts.voice';

UPDATE settings
SET description = 'API key for the fallback speech endpoint; a ${VAR} placeholder resolves from settings then environment (e.g. ${GROQ_API_KEY}). Keys for role="tts" models live on the Models page instead, where they are encrypted at rest — prefer that for anything long-lived.'
WHERE key = 'tts.api_key';
