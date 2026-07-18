package com.axon.voice.audio

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.io.File

/**
 * Plays a synthesized reply file; when server TTS is unavailable, falls back
 * to Android's built-in TextToSpeech engine — the same "never silent" rule as
 * the dashboard's browser speechSynthesis fallback.
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
}
