package com.axon.voice.audio

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.concurrent.thread
import kotlin.math.sqrt

/**
 * Decides when a voice capture should stop. Thresholds are a direct port of
 * axon-ui/src/lib/wakeword.js: stop after 1.4s of quiet once speech was heard,
 * give up if nothing is said within [noSpeechTicks]*100ms, hard cap at 12s.
 * The follow-up window raises [speechRms] to ~2x so room-level chatter from
 * bystanders cancels the window instead of being sent as a command.
 */
class SilenceWatcher(
    private val speechRms: Double = RMS_SPEECH,
    private val quietTicks: Int = QUIET_TICKS,
    private val noSpeechTicks: Int = NO_SPEECH_TICKS,
    private val maxTicks: Int = MAX_TICKS,
) {
    companion object {
        const val RMS_SPEECH = 0.012
        const val QUIET_TICKS = 14
        const val NO_SPEECH_TICKS = 50
        const val MAX_TICKS = 120
        const val FOLLOWUP_RMS = 0.025
    }

    var hadSpeech = false
        private set
    private var quiet = 0
    private var ticks = 0

    /** Feed one ~100ms RMS tick; returns true when the capture should stop. */
    fun tick(rms: Double): Boolean {
        ticks++
        if (rms > speechRms) {
            hadSpeech = true
            quiet = 0
        } else if (hadSpeech) {
            quiet++
        }
        return (hadSpeech && quiet >= quietTicks) ||
            (!hadSpeech && ticks >= noSpeechTicks) ||
            ticks >= maxTicks
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
        val raw = synchronized(pcm) { pcm.toByteArray() }
        return wavBytes(raw)
    }
}
