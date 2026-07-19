package com.axon.voice.audio

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaPlayer
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
    private var mp: MediaPlayer? = null
    private var fallback: TextToSpeech? = null
    private var fallbackReady = false

    init {
        fallback = TextToSpeech(ctx.applicationContext) { status ->
            fallbackReady = status == TextToSpeech.SUCCESS
        }
    }

    val playing: Boolean
        get() = mp?.isPlaying == true

    fun play(file: File, onDone: () -> Unit) {
        stop()
        val player = MediaPlayer()
        mp = player
        try {
            player.setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ASSISTANT)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
            )
            player.setDataSource(file.absolutePath)
            player.setOnCompletionListener {
                cleanup()
                onDone()
            }
            player.setOnErrorListener { _, _, _ ->
                cleanup()
                onDone()
                true
            }
            player.prepare()
            player.start()
        } catch (_: Exception) {
            cleanup()
            onDone()
        }
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

        /** Used by the wake barge-in detector: true until playback truly ends. */
        val active: Boolean
            get() = !closed || !idle || queue.isNotEmpty()
    }

    /**
     * Begin a streamed reply. Retires any previous stream but deliberately does
     * NOT silence the speaker: this is called the moment the task is sent, and
     * the first sentence is still a synthesis round-trip away. Cutting the audio
     * here would kill the "thinking" filler the instant it started and leave
     * dead air for the whole agent run. The speaker is taken over in
     * [playNextLocked], when this reply actually has something to say.
     */
    fun beginStream(onDone: () -> Unit): Stream {
        synchronized(streamLock) {
            currentStream?.let { it.closed = true; it.queue.clear() }
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
            if (s.idle) playNextLocked(s)
        }
    }

    /** Flush the stream — nothing more will be enqueued. If the queue has
     *  drained (or nothing was ever enqueued), [onDone] fires now; otherwise
     *  it fires when the remaining files finish playing. Idempotent. */
    fun finalizeStream(s: Stream) {
        synchronized(streamLock) {
            if (s.closed) return
            s.closed = true
            if (s.idle && s.queue.isEmpty()) finishLocked(s)
        }
    }

    /** Drop everything queued for [s] right now (barge-in). onDone will NOT
     *  fire — the caller is taking over the speaker. */
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
        val file = s.queue.poll() ?: return
        s.idle = false
        // Anything still coming out of the speaker — the thinking filler, an
        // ack tail — yields now that the reply can actually speak.
        stopPlayback()
        val player = MediaPlayer()
        mp = player
        try {
            player.setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ASSISTANT)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
            )
            player.setDataSource(file.absolutePath)
            player.setOnCompletionListener {
                cleanup()
                onStreamFileDone(s)
            }
            player.setOnErrorListener { _, _, _ ->
                cleanup()
                onStreamFileDone(s)
                true
            }
            player.prepare()
            player.start()
        } catch (_: Exception) {
            cleanup()
            onStreamFileDone(s)
        }
    }

    private fun onStreamFileDone(s: Stream) {
        synchronized(streamLock) {
            s.idle = true
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
        mp?.let {
            runCatching { it.stop() }
            it.release()
        }
        mp = null
        runCatching { fallback?.stop() }
    }

    private fun cleanup() {
        mp?.release()
        mp = null
    }

    fun release() {
        stop()
        fallback?.shutdown()
        fallback = null
    }

    private val streamLock = Any()
    private var currentStream: Stream? = null
}
