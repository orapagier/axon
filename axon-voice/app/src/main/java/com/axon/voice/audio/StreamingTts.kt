package com.axon.voice.audio

import com.axon.voice.api.AxonClient
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/**
 * Bridges a streaming LLM reply (token frames) to incremental TTS so the user
 * hears the first sentence as soon as it's generated, instead of waiting for
 * the whole reply to be synthesized and downloaded — the "reads it silently,
 * then reads it aloud" delay.
 *
 * Strategy: accumulate incoming text; whenever a sentence boundary (`. ` `! `
 * `? ` or a newline) is reached, synthesize just that sentence via server TTS
 * and enqueue it for back-to-back playback. Whatever is left when [finish] is
 * called is flushed as a final chunk. If the very first synth attempt fails we
 * bail to Android's built-in TTS on the whole accumulated text so we're never
 * silent — same "never silent" rule as the single-blob path.
 *
 * All synth runs on a single-thread executor, so:
 *  - the caller thread (WS listener / main) is never blocked by network I/O,
 *  - sentence files are synthesized and enqueued in order (back-to-back play),
 *  - [finish] is race-free: it submits behind any in-flight synths, so the
 *    stream can't be marked done before all its files are enqueued.
 */
class StreamingTts(
    private val player: TtsPlayer,
    private val client: AxonClient,
    cacheDir: File,
    /** Distinct filename prefix so concurrent wake/UI streams don't collide. */
    private val filePrefix: String,
    private val onDone: () -> Unit,
) {
    private val buf = StringBuilder()

    /** Every token ever appended. Separate from [buf], which is drained as
     *  sentences are split off it — the built-in-TTS fallback has to speak the
     *  whole reply, not just the trailing partial sentence left over. */
    private val full = StringBuilder()
    private val lock = Any()

    private val stream: TtsPlayer.Stream = player.beginStream(onDone)

    /** Distinct counter so every sentence file is unique and survives the
     *  short window between enqueue and playback. */
    private val counter = AtomicInteger(0)
    private val dir = cacheDir
    @Volatile
    private var anyServerTts = false
    @Volatile
    private var abandoned = false

    private val synth = Executors.newSingleThreadExecutor { r ->
        Thread(r, "$filePrefix-tts").apply { isDaemon = true }
    }

    /** Feed one token of the agent's reply. Thread-safe; called from the WS
     *  listener thread (wake path) or the main thread (UI path). Returns
     *  immediately — synthesis happens on a worker. */
    fun append(token: String) {
        if (token.isEmpty() || abandoned) return
        val ready = synchronized(lock) {
            buf.append(token)
            full.append(token)
            consumeSentences()
        }
        for (sentence in ready) synth.execute { synthAndEnqueue(sentence) }
    }

    /** Flush whatever is buffered as a final chunk and mark the stream closed.
     *  Idempotent. The [onDone] callback fires once playback drains. Safe to
     *  call from any thread; the flush is sequenced behind in-flight synths. */
    fun finish() {
        if (abandoned) return
        synth.execute {
            val final = synchronized(lock) { takeBuffered() }
            if (final.isNotEmpty()) synthAndEnqueue(final)
            player.finalizeStream(stream)
            synth.shutdown() // all synth work for this reply is done
        }
    }

    /** Drop everything immediately (e.g. barge-in). Does not fire onDone. */
    fun abort() {
        abandoned = true
        synchronized(lock) { buf.setLength(0) }
        synth.execute {
            player.abortStream(stream)
            synth.shutdown()
        }
    }

    /** Read-only snapshot of everything appended so far — for self-echo
     *  detection (wake path) and the transcript bubble (UI path). */
    fun accumulated(): String = synchronized(lock) { full.toString() }

    /** True once any reply text has been fed in. Lets the caller tell a real
     *  streamed reply apart from a stream that never received a token frame,
     *  which must fall back to synthesizing `full_text` in one blob rather
     *  than finalizing an empty stream (i.e. saying nothing at all). */
    val hasContent: Boolean
        get() = synchronized(lock) { full.isNotEmpty() }

    // ── internals ───────────────────────────────────────────────────────────

    /** Pulls any complete sentences out of [buf], leaving a partial trailing
     *  sentence in place. Returns them in order. Caller holds [lock]. */
    private fun consumeSentences(): List<String> {
        val out = ArrayList<String>()
        while (true) {
            val idx = nextBoundary(buf)
            if (idx < 0) break
            val sentence = buf.substring(0, idx)
            buf.delete(0, idx)
            val trimmed = sentence.trim()
            if (trimmed.isNotEmpty()) out.add(trimmed)
        }
        return out
    }

    /** Index just past the first sentence boundary, or -1 if none yet. */
    private fun nextBoundary(s: StringBuilder): Int {
        var i = 0
        val n = s.length
        while (i < n) {
            val c = s[i]
            if (c == '\n' || c == '\r') return i + 1
            if (c == '.' || c == '!' || c == '?') {
                var j = i + 1
                // include any trailing quotes/brackets then whitespace
                while (j < n && !s[j].isWhitespace()) {
                    val cj = s[j]
                    if (cj != '"' && cj != '\'' && cj != ']' && cj != ')' && cj != '”' && cj != '’') break
                    j++
                }
                if (j < n && s[j].isWhitespace()) return j + 1
                if (j >= n) return -1 // boundary at EOF — let caller flush via finish()
            }
            i++
        }
        return -1
    }

    private fun takeBuffered(): String {
        val s = buf.toString().trim()
        buf.setLength(0)
        return s
    }

    private fun synthAndEnqueue(text: String) {
        if (abandoned) return
        val file = File(dir, "$filePrefix-${counter.incrementAndGet()}.audio")
        val ok = runCatching { client.speech(text, file) }.getOrDefault(false)
        if (abandoned) { file.delete(); return }
        if (ok && file.length() > 0) {
            anyServerTts = true
            player.enqueueStreamFile(stream, file)
            return
        }
        file.delete()
        // If server TTS has never worked for this reply, abandon streaming and
        // speak the whole thing with the built-in engine (never silent). Once
        // we've spoken even one sentence via the server we keep streaming and
        // just drop this one bad chunk.
        if (!anyServerTts && !abandoned) {
            // One fallback per reply: further queued sentences must not each
            // start their own utterance (and fire onDone again).
            abandoned = true
            val whole = accumulated().trim()
            synchronized(lock) { buf.setLength(0) }
            player.abortStream(stream)
            // onDone MUST fire on every exit here. abortStream closed the
            // stream, so the pending finish() can no longer finalize it — if we
            // returned silently the wake service would block on its latch until
            // the 310s timeout instead of moving to the follow-up window.
            if (whole.isNotEmpty()) player.speakFallback(whole, onDone) else onDone()
        }
    }
}
