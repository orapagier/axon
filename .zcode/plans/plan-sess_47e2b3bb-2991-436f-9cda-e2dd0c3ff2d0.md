## Goal
Bring the Android wake ack path back in sync with the dashboard it mirrors (randomized wake acks, 3-tier playback), and add a short spoken "thinking" filler after the user's command lands but before the reply arrives.

## Phrase sets
- **Wake acks** (mirror of `axon-ui/src/lib/voiceprompts.js:13`): `["Yes?", "Mm-hmm?", "I'm listening."]` — picked uniformly at random per wake.
- **Thinking fillers** (new, no dashboard precedent): `["Let me check.", "Working on it.", "One sec.", "On it."]` — picked at random, played once after transcribe succeeds, before the WS reply arrives.
- **Follow-up ack**: unchanged (`"Anything else?"`, fixed — matches dashboard).

## Files

### 1. `axon-voice/app/src/main/java/com/axon/voice/audio/VoicePrompts.kt` (NEW)
A small Kotlin mirror of `voiceprompts.js`:
- `val WAKE_ACKS = listOf("Yes?", "Mm-hmm?", "I'm listening.")`
- `val THINKING_FILLERS = listOf("Let me check.", "Working on it.", "One sec.", "On it.")`
- `fun randomWakeAck(): String` and `fun randomFiller(): String` via `Random.nextInt(...)`.
- Pure data/helpers, no Android imports — easy to unit-test, matches dashboard naming.

### 2. `axon-voice/app/src/main/java/com/axon/voice/audio/TtsPlayer.kt` (EDIT)
Expose the missing built-in-TTS tier for acks so short prompts aren't silent when server TTS fails:
- Extract `speakFallback(text, onDone)` to a public `speak(text, onDone)` that already routes server-file→built-in-TTS; or keep `speakFallback` as-is and add a thin public alias. Prefer minimal: rename the existing private `speakFallback` intent into a public `speakOrFallback(file?, text, onDone)` so `playAckBlocking` can use the same 3-tier chain as `speakBlocking`.
- Keep the existing `play(file, onDone)` for server-TTS-file playback (used by replies).

### 3. `axon-voice/app/src/main/java/com/axon/voice/wake/WakeWordService.kt` (EDIT)
**A. Prefetch all ack phrases, not just two** — mirror `voiceprompts.js:27-45`. Replace the two hardcoded `ackYes`/`ackMore` `File` fields with:
- `private val wakeAckFiles = mutableMapOf<String, File>()` (phrase → cached audio)
- `private var ackMore: File? = null` (follow-up — single phrase, stays a File)
- `prefetchAcks()` iterates `WAKE_ACKS + "Anything else?"`, fetches each via `client.speech(text, file)` like today, but **retries on next call** if a fetch failed (mirror the dashboard's `inflight`/`cache` resilience). Simplest correct form: skip phrases whose cached file exists and is non-empty; attempt the rest; delete-on-failure so they retry next service start. (Current code already deletes-on-failure; we just loop over the full set.)
- Call `prefetchAcks()` from `runLoop()` where it's called today (line 160).

**B. `playAckBlocking()` becomes 3-tier** (matches `speakBlocking`'s pattern + dashboard `playPrompt`):
- Pick the phrase at random inside `interact()` (wake) / fixed "Anything else?" (follow-up).
- If cached server-TTS file exists & non-empty → `player.play(file)`.
- Else → `player.speakFallback(phrase)` (built-in TTS) — the tier that's currently missing for acks.
- Else (built-in TTS not ready) → `Sound.chime(soft)` as last resort (existing behavior).
- Change signature: `playAckBlocking(phrase: String, cachedFile: File?, soft: Boolean)`.

**C. `interact()` changes:**
- Wake branch (`first == true`): `val phrase = randomWakeAck(); playAckBlocking(phrase, wakeAckFiles[phrase], soft = false)`.
- Follow-up branch: `playAckBlocking("Anything else?", ackMore, soft = true)`.
- **Thinking filler:** after `text = client.transcribe(wav)` succeeds and passes `isSelfEcho`, but **before** `sendAndAwait(text)`, call `playFillerNonBlocking()` — picks a random filler and speaks it without blocking the thread (fire-and-forget on a short-lived thread or via a `once` flag). Rationale: we don't want to delay the WS send, and we don't want the filler to still be talking when the reply arrives (the reply playback calls `TtsPlayer.stop()` first, which will cut the filler off naturally).
  - Concretely: `private fun playFillerNonBlocking()` spawns a `thread(name="axon-filler"){ player?.speakFallback(randomFiller()){} }`. When `speakBlocking(reply)` runs later, `player.play()` → `stop()` (TtsPlayer.kt:76) stops both MediaPlayer and the built-in TTS, so overlap is impossible.

**D. No changes** to: `runLoop`, `openRecord`, `capture`, `drain`, `sendAndAwait`, `onWsEvent`, notification code, `Sound`, `WavRecorder`, `MicEffects` (PTT path), `MainActivity`.

## Concurrency / safety review
- `prefetchAcks()` runs once on the wake worker thread before the loop (line 160) — same as today, no new threading.
- `wakeAckFiles` is only read/written from the wake worker thread (`prefetchAcks` writes in `runLoop` setup; reads in `interact()` on the same thread). No synchronization needed.
- Filler thread vs. reply playback: `TtsPlayer.play()` calls `stop()` first (line 30), and `stop()` calls `fallback?.stop()` (line 82) — so launching the reply will interrupt any in-flight filler on the same `TtsPlayer` instance. Safe by design, no new locks.
- Wake ack plays before the drain+capture (unchanged ordering), so it can't leak into the command capture any more than today (the existing `AUDIO_SETTLE_MS`/`drain`/`minSpeechTicks` defenses still apply).

## Verification
- `cd axon-voice && ./gradlew assembleDebug` must pass (this also compiles `:app:compileDebugKotlin`, proving the refactor is sound).
- On-device: say "Hey Axon" 5–10×; confirm the ack varies among the three phrases. Give a command; confirm a filler plays and then gets cut off cleanly by the reply. Trigger the follow-up window; confirm "Anything else?" plays (fixed phrase).

## Out of scope (separate tickets)
- The `isSelfEcho()` ≤12-word subset over-match flagged earlier.
- Generalizing the dashboard's `voiceprompts.js` cache-retry semantics to a fuller form (current delete-on-failure + service-restart retry is adequate).