package com.axon.androidcompanion.audio

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
 *
 * @param cleanSignal skip [MicEffects] (AEC/NS) — set for wake-word enrollment
 *   captures, which must match the untouched signal [WakeWordService][com.axon.androidcompanion.wake.WakeWordService]
 *   feeds the detector at runtime; those effects strip the quiet signal a
 *   wake model gets matched against. Dictation/push-to-talk capture leaves
 *   this off (the default) since echo cancellation helps there instead.
 */
class WavRecorder(private val cleanSignal: Boolean = false) {
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

        /** The inverse of [wavBytes]: raw PCM16 samples from a WAV this class
         *  produced (44-byte header, no extra chunks). */
        fun pcmFromWav(wav: ByteArray): ShortArray {
            val samples = ShortArray((wav.size - 44) / 2)
            ByteBuffer.wrap(wav, 44, wav.size - 44).order(ByteOrder.LITTLE_ENDIAN)
                .asShortBuffer().get(samples)
            return samples
        }

        /**
         * Trim a WAV this class produced down to just its spoken region — from
         * the first ~10ms window whose RMS clears [speechRms] to the last, padded
         * by [padMs] either side. An enrollment take otherwise carries the
         * recorder's reaction-time lead-in and a stretch of trailing quiet;
         * rustpotter's few-shot "reference" builder bakes whatever silence is in
         * a clip straight into the MFCC template and never trims it, so those
         * padded, variable-offset clips make the on-device model far pickier to
         * trigger than the dashboard's tight manual-stop takes. Returns null when
         * nothing clears the bar (a clip of pure silence) so the caller can
         * reject the take.
         */
        fun trimToSpeech(
            wav: ByteArray,
            speechRms: Double = SilenceWatcher.RMS_SPEECH,
            padMs: Int = 150,
        ): ByteArray? {
            val pcm = pcmFromWav(wav)
            if (pcm.isEmpty()) return null
            val win = SAMPLE_RATE / 100 // ~10ms windows
            var firstLoud = -1
            var lastLoud = -1
            var i = 0
            while (i < pcm.size) {
                val end = minOf(i + win, pcm.size)
                var acc = 0.0
                for (j in i until end) {
                    val s = pcm[j] / 32768.0
                    acc += s * s
                }
                if (sqrt(acc / (end - i)) > speechRms) {
                    if (firstLoud < 0) firstLoud = i
                    lastLoud = end
                }
                i += win
            }
            if (firstLoud < 0) return null
            val pad = SAMPLE_RATE * padMs / 1000
            val start = maxOf(0, firstLoud - pad)
            val stop = minOf(pcm.size, lastLoud + pad)
            val out = ByteBuffer.allocate((stop - start) * 2).order(ByteOrder.LITTLE_ENDIAN)
            for (k in start until stop) out.putShort(pcm[k])
            return wavBytes(out.array())
        }
    }

    private var record: AudioRecord? = null
    private var effects: MicEffects? = null
    private var worker: Thread? = null

    @Volatile
    private var running = false
    private val pcm = ByteArrayOutputStream()

    val isRecording: Boolean get() = running

    /** Starts capturing; [onTick] fires on the recorder thread every ~100ms. */
    @SuppressLint("MissingPermission")
    fun start(onTick: (Double) -> Unit) {
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
        effects = if (cleanSignal) null else MicEffects(rec.audioSessionId)
        pcm.reset()
        running = true
        rec.startRecording()
        worker = thread(name = "axon-wav-rec") {
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
