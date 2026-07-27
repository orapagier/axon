package com.axon.androidcompanion.audio

import android.content.Context
import android.media.AudioAttributes
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.io.File
import java.util.ArrayDeque

/**
 * Plays a synthesized reply file; when server TTS is unavailable, falls back
 * to Android's built-in TextToSpeech engine — the same "never silent" rule as
 * the dashboard's browser speechSynthesis fallback.
 *
 * Two playback modes:
 *  - play(file)           — one-shot (ack phrases, the legacy single-blob path).
 *  - beginStream/...      — back-to-back playback of a sequence of per-sentence
 *                           files so a streamed reply starts speaking as soon as
 *                           the first sentence is synthesized, not after the
 *                           whole reply is downloaded.
 */
class TtsPlayer(ctx: Context) {
    private var current: PcmPlayback? = null
    private var fallback: TextToSpeech? = null
    private var fallbackReady = false

    /** Live 0..1 RMS of whatever is playing (-1 when it stops), forwarded from
     *  [PcmPlayback]. The wake service points this at the voice orb so the
     *  SPEAKING orb reacts to the real reply; unset elsewhere (a cheap no-op). */
    @Volatile
    var onLevel: ((Float) -> Unit)? = null

    /** Last real (non-negative) level seen this stream, or -1 before any has.
     *  Each queued sentence file's [PcmPlayback] emits -1 the instant it ends
     *  (see its class doc), before [playNextLocked] even runs to find out
     *  whether another sentence is queued right behind it — a real gap
     *  between sentences and the reply having genuinely finished look
     *  identical at that moment. [playNextLocked] re-asserts this value the
     *  instant it decides to start the next file, so the orb sees "still
     *  speaking" again immediately instead of reading a full decode/
     *  codec-setup's worth of silence as the reply having stopped. */
    @Volatile
    private var lastLevel = -1f

    private val speechAttrs = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ASSISTANT)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()

    init {
        fallback = TextToSpeech(ctx.applicationContext) { status ->
            fallbackReady = status == TextToSpeech.SUCCESS
        }
    }

    val playing: Boolean
        get() = current?.playing == true

    fun play(file: File, onDone: () -> Unit) {
        stop()
        startFile(file, onDone)
    }

    private fun startFile(file: File, onDone: () -> Unit) {
        try {
            val pb = newPlayback(file) { onDone() }
            current = pb
            pb.start()
        } catch (_: Exception) {
            onDone()
        }
    }

    /** A [PcmPlayback] that retires itself as the sink on completion, then runs
     *  [after] — the direct analogue of the old MediaPlayer completion/error
     *  path (both of which did cleanup + onDone). A decode failure lands here
     *  too, so a bad file advances the queue just like a finished one. */
    private fun newPlayback(file: File, after: () -> Unit): PcmPlayback {
        var pb: PcmPlayback? = null
        val p = PcmPlayback(file, speechAttrs, { l -> if (l >= 0f) lastLevel = l; onLevel?.invoke(l) }) {
            pb?.let { cleanup(it) }
            after()
        }
        pb = p
        return p
    }

    // ── Streaming reply playback ────────────────────────────────────────────

    /** A reply being streamed out as it is synthesized. Files enqueued via
     *  [enqueueStreamFile] play back-to-back; [finalizeStream] flushes the
     *  leftover buffered text and fires [onDone] once everything has played. */
    class Stream(internal val onDone: () -> Unit) {
        internal val queue = ArrayDeque<File>()
        internal var closed = false          // finalizeStream called
        @Volatile
        internal var idle = true             // no file currently playing
        @Volatile
        internal var paused = false          // barge hold — no new file starts
        /** The file [playNextLocked] most recently started, so a pause can push
         *  it back to the head of the queue and replay that whole sentence on
         *  resume (per-sentence is the finest resume unit — [PcmPlayback] has no
         *  mid-file resume). Null while nothing is playing. */
        internal var currentFile: File? = null
    }

    /**
     * Begin a streamed reply. Retires any previous stream but deliberately does
     * NOT silence the speaker: this is called the moment the task is sent, and
     * the first sentence is still a synthesis round-trip away, so anything still
     * playing (an ack tail, a read-aloud the user started) should finish rather
     * than be chopped for a reply that has nothing to say yet. The speaker is
     * taken over in [playNextLocked], once this reply actually does.
     */
    fun beginStream(onDone: () -> Unit): Stream {
        synchronized(streamLock) {
            currentStream?.let { it.closed = true; it.queue.clear() }
            lastLevel = -1f // no carry-over from whatever last played
            val s = Stream(onDone)
            currentStream = s
            return s
        }
    }

    /** Enqueue a synthesized sentence file for back-to-back playback. Safe to
     *  call from any thread; starts playback immediately if the sink is idle. */
    fun enqueueStreamFile(s: Stream, file: File) {
        synchronized(streamLock) {
            if (s.closed || file.length() == 0L) return
            s.queue.add(file)
            if (s.idle && !s.paused) playNextLocked(s)
        }
    }

    /** Pause a streamed reply at the current sentence for a barge check: silence
     *  the speaker now, and push the interrupted sentence back to the head of
     *  the queue so [resumeStream] replays it whole. Newly-synthesized sentences
     *  keep queuing behind it while paused. No-op unless [s] is the live stream
     *  and not already paused. onDone does NOT fire — the reply isn't finished. */
    fun pauseStream(s: Stream) {
        synchronized(streamLock) {
            if (currentStream !== s || s.paused) return
            s.paused = true
            // stopPlayback cancels the current PcmPlayback WITHOUT firing its
            // onEnd, so onStreamFileDone won't run for the interrupted sentence
            // — we re-queue it and mark idle by hand.
            s.currentFile?.let { s.queue.addFirst(it) }
            s.currentFile = null
            stopPlayback()
            s.idle = true
        }
    }

    /** Undo [pauseStream]: resume back-to-back playback from the head of the
     *  queue (the replayed sentence first). If the reply had already finished
     *  streaming and its queue drained while paused, fire onDone now. */
    fun resumeStream(s: Stream) {
        synchronized(streamLock) {
            if (currentStream !== s || !s.paused) return
            s.paused = false
            onLevel?.invoke(lastLevel) // orb back to "speaking" immediately
            if (s.idle) {
                if (s.queue.isNotEmpty()) playNextLocked(s)
                else if (s.closed) finishLocked(s)
            }
        }
    }

    /** Flush the stream — nothing more will be enqueued. If the queue has
     *  drained (or nothing was ever enqueued), [onDone] fires now; otherwise
     *  it fires when the remaining files finish playing. Idempotent. */
    fun finalizeStream(s: Stream) {
        synchronized(streamLock) {
            if (s.closed) return
            s.closed = true
            // While paused for a barge check, the queue holds the replayed
            // sentence — don't fire onDone; resumeStream will when it drains.
            if (s.idle && !s.paused && s.queue.isEmpty()) finishLocked(s)
        }
    }

    /** Drop everything queued for [s] right now (a cancelled run, or a fresh
     *  reply taking over). onDone will NOT fire — the caller is taking over
     *  the speaker. */
    fun abortStream(s: Stream) {
        val owned = synchronized(streamLock) {
            s.closed = true
            s.queue.clear()
            val cur = currentStream === s
            if (cur) currentStream = null
            cur
        }
        // Only silence the speaker while this stream still owns it. A late
        // abort from a finished reply must not cut off the next one.
        if (owned) stopPlayback()
    }

    private fun playNextLocked(s: Stream) {
        if (s.paused) return
        val file = s.queue.poll() ?: return
        s.currentFile = file
        s.idle = false
        // Anything still coming out of the speaker — an ack tail, a read-aloud
        // in progress — yields now that the reply can actually speak.
        stopPlayback()
        // The previous file's PcmPlayback already emitted -1 (onEnd fires
        // after onLevel(-1), and this call happens as a result of that onEnd)
        // — re-assert the last real level now that we know another sentence
        // is actually coming, so the orb sees "still speaking" again
        // immediately rather than reading a full decode/codec-setup's worth
        // of silence as the reply having stopped.
        onLevel?.invoke(lastLevel)
        try {
            val pb = newPlayback(file) { onStreamFileDone(s) }
            current = pb
            pb.start()
        } catch (_: Exception) {
            onStreamFileDone(s)
        }
    }

    private fun onStreamFileDone(s: Stream) {
        synchronized(streamLock) {
            s.idle = true
            s.currentFile = null // this sentence finished naturally; nothing to replay
            if (currentStream !== s) return@synchronized
            if (s.paused) return@synchronized // resumeStream will pick the queue back up
            if (s.queue.isNotEmpty()) {
                playNextLocked(s)
            } else if (s.closed) {
                finishLocked(s)
            }
        }
    }

    private fun finishLocked(s: Stream) {
        if (currentStream === s) currentStream = null
        s.onDone.invoke()
    }

    /** Built-in engine fallback; onDone fires when speech ends (or fails). */
    fun speakFallback(text: String, onDone: () -> Unit) {
        val tts = fallback
        if (tts == null || !fallbackReady) {
            onDone()
            return
        }
        tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
            override fun onStart(utteranceId: String?) {}
            override fun onDone(utteranceId: String?) = onDone()

            @Deprecated("Deprecated in Java")
            override fun onError(utteranceId: String?) = onDone()
        })
        val r = tts.speak(text, TextToSpeech.QUEUE_FLUSH, null, "axon-reply")
        if (r != TextToSpeech.SUCCESS) onDone()
    }

    fun stop() {
        synchronized(streamLock) {
            currentStream?.let { it.closed = true; it.queue.clear() }
            currentStream = null
        }
        stopPlayback()
    }

    /** Silence the speaker without touching stream bookkeeping. */
    private fun stopPlayback() {
        current?.stop() // cancels playback and releases its AudioTrack; no onEnd
        current = null
        runCatching { fallback?.stop() }
    }

    /** Retire [pb] — but only drop it as the sink if it still is the sink. A
     *  one-shot that finishes after the reply took the speaker must not null out
     *  the reply's playback. [PcmPlayback] frees its own AudioTrack on the way
     *  out, so there is nothing else to release here. */
    private fun cleanup(pb: PcmPlayback) {
        if (current === pb) current = null
    }

    fun release() {
        stop()
        fallback?.shutdown()
        fallback = null
    }

    private val streamLock = Any()
    private var currentStream: Stream? = null
}
