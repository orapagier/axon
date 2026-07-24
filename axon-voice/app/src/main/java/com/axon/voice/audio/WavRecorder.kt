package com.axon.voice.audio

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.concurrent.thread
import kotlin.math.sqrt

/**
 * Decides when a voice capture should stop. Thresholds are a direct port of
 * axon-ui/src/lib/wakeword.js: stop after 1.4s of quiet once speech was heard,
 * give up if nothing is said within [noSpeechTicks]*100ms. [maxTicks] is only a
 * runaway safety net (a noisy room that never falls quiet enough to trip the
 * quiet counter), so it is 60s — long enough that a real voice message is never
 * cut off mid-sentence; normal speech still ends 1.4s after the talker stops.
 * The follow-up window raises [speechRms] to ~2x so room-level chatter from
 * bystanders cancels the window instead of being sent as a command.
 *
 * [minSpeechTicks] consecutive loud ticks are required before a capture counts
 * as speech. One tick used to be enough on a wake capture, so a cough, a click,
 * the tail of our own TTS, or a shout from another room latched [hadSpeech] and
 * the clip was uploaded — where a transcriber handed near-silence invents a
 * stock phrase ("Thank you.") and it gets sent as a command. Real speech holds
 * the level well past 300ms; impulse noise does not. Mirrored in
 * axon-ui/src/lib/wakeword.js — keep the two in step.
 */
class SilenceWatcher(
    private val speechRms: Double = RMS_SPEECH,
    private val quietTicks: Int = QUIET_TICKS,
    private val noSpeechTicks: Int = NO_SPEECH_TICKS,
    private val maxTicks: Int = MAX_TICKS,
    private val minSpeechTicks: Int = SPEECH_ONSET_TICKS,
) {
    companion object {
        const val RMS_SPEECH = 0.012
        const val QUIET_TICKS = 14
        const val NO_SPEECH_TICKS = 50
        const val MAX_TICKS = 600
        const val FOLLOWUP_RMS = 0.025
        const val SPEECH_ONSET_TICKS = 3
    }

    var hadSpeech = false
        private set
    private var quiet = 0
    private var ticks = 0
    private var loud = 0

    /** Feed one ~100ms RMS tick; returns true when the capture should stop. */
    fun tick(rms: Double): Boolean {
        ticks++
        if (rms > speechRms) {
            loud++
            if (loud >= minSpeechTicks) hadSpeech = true
            quiet = 0
        } else {
            loud = 0
            if (hadSpeech) quiet++
        }
        return (hadSpeech && quiet >= quietTicks) ||
            (!hadSpeech && ticks >= noSpeechTicks) ||
            ticks >= maxTicks
    }
}

/**
 * Best-effort hardware echo cancellation + noise suppression on a capture
 * session — the platform stand-in for the `echoCancellation: true` constraint
 * the dashboard's browser capture always has, so the phone's own TTS output
 * is less likely to land in a capture. Not every device implements the
 * effects; callers must not rely on them alone.
 */
class MicEffects(sessionId: Int) {
    private val aec: AcousticEchoCanceler? = if (AcousticEchoCanceler.isAvailable()) {
        runCatching { AcousticEchoCanceler.create(sessionId)?.apply { enabled = true } }.getOrNull()
    } else {
        null
    }
    private val ns: NoiseSuppressor? = if (NoiseSuppressor.isAvailable()) {
        runCatching { NoiseSuppressor.create(sessionId)?.apply { enabled = true } }.getOrNull()
    } else {
        null
    }

    fun release() {
        runCatching { aec?.release() }
        runCatching { ns?.release() }
    }
}

/**
 * Records 16kHz mono PCM16 off its own thread, reporting an RMS level
 * (0..1 float scale, same as the web AnalyserNode) every ~100ms.
 * stop() returns a complete in-memory WAV ready for /api/audio/transcribe.
 */
class WavRecorder {
    companion object {
        const val SAMPLE_RATE = 16000
        private const val TICK_SAMPLES = SAMPLE_RATE / 10

        /** Wrap raw 16k mono PCM16 bytes in a standard 44-byte WAV header. */
        fun wavBytes(pcm: ByteArray, sampleRate: Int = SAMPLE_RATE): ByteArray {
            val byteRate = sampleRate * 2
            val header = ByteBuffer.allocate(44).order(ByteOrder.LITTLE_ENDIAN)
            header.put("RIFF".toByteArray())
            header.putInt(36 + pcm.size)
            header.put("WAVE".toByteArray())
            header.put("fmt ".toByteArray())
            header.putInt(16)
            header.putShort(1) // PCM
            header.putShort(1) // mono
            header.putInt(sampleRate)
            header.putInt(byteRate)
            header.putShort(2) // block align
            header.putShort(16) // bits per sample
            header.put("data".toByteArray())
            header.putInt(pcm.size)
            return header.array() + pcm
        }
    }

    private var record: AudioRecord? = null
    private var effects: MicEffects? = null
    private var worker: Thread? = null

    @Volatile
    private var running = false
    private val pcm = ByteArrayOutputStream()

    val isRecording: Boolean get() = running

    /** Starts capturing; [onTick] fires on the recorder thread every ~100ms.
     *  When [preroll] is supplied (a confirmed barge-in handing off to
     *  dictation), it's written to the front of the recording and its own RMS
     *  is fed through [onTick] first — the pre-roll covers what the user said
     *  in the ~300-600ms it took the barge-in to confirm, so a caller-owned
     *  [SilenceWatcher] behind [onTick] sees it as speech before a single live
     *  frame is read, exactly as if it had been captured live. */
    @SuppressLint("MissingPermission")
    fun start(preroll: ByteArray? = null, onTick: (Double) -> Unit) {
        if (running) return
        val minBuf = AudioRecord.getMinBufferSize(
            SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT
        )
        val rec = AudioRecord(
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            maxOf(minBuf * 2, SAMPLE_RATE * 2)
        )
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            throw IllegalStateException("microphone unavailable")
        }
        record = rec
        effects = MicEffects(rec.audioSessionId)
        pcm.reset()
        if (preroll != null && preroll.isNotEmpty()) {
            synchronized(pcm) { pcm.write(preroll) }
        }
        running = true
        rec.startRecording()
        worker = thread(name = "axon-wav-rec") {
            if (preroll != null && preroll.isNotEmpty()) feedPrerollTicks(preroll, onTick)
            val buf = ShortArray(TICK_SAMPLES)
            while (running) {
                val n = rec.read(buf, 0, buf.size)
                if (n <= 0) continue
                val bytes = ByteBuffer.allocate(n * 2).order(ByteOrder.LITTLE_ENDIAN)
                var acc = 0.0
                for (i in 0 until n) {
                    bytes.putShort(buf[i])
                    val s = buf[i] / 32768.0
                    acc += s * s
                }
                synchronized(pcm) { pcm.write(bytes.array()) }
                onTick(sqrt(acc / n))
            }
        }
    }

    /** Replays [preroll] (raw 16k mono PCM16) as ~100ms RMS ticks through
     *  [onTick], the same cadence live capture uses. */
    private fun feedPrerollTicks(preroll: ByteArray, onTick: (Double) -> Unit) {
        val tickBytes = TICK_SAMPLES * 2 // 100ms of 16-bit mono
        var off = 0
        while (off < preroll.size && running) {
            val pairs = minOf(tickBytes, preroll.size - off) / 2 // whole samples in this tick
            if (pairs == 0) break
            var acc = 0.0
            for (k in 0 until pairs) {
                val i = off + k * 2
                val s = ((preroll[i].toInt() and 0xff) or (preroll[i + 1].toInt() shl 8)).toShort()
                val f = s / 32768.0
                acc += f * f
            }
            onTick(sqrt(acc / pairs))
            off += pairs * 2
        }
    }

    fun stop(): ByteArray {
        running = false
        worker?.join(500)
        worker = null
        record?.let {
            runCatching { it.stop() }
            it.release()
        }
        record = null
        effects?.release()
        effects = null
        val raw = synchronized(pcm) { pcm.toByteArray() }
        return wavBytes(raw)
    }
}
