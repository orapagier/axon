package com.axon.voice.audio

import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.sqrt

/**
 * Detects a low-pitched (male) voice in the mic WHILE a higher-pitched (female)
 * TTS reply is playing — without any echo cancellation. It watches only the
 * ~90-160 Hz band, where a male speaking fundamental lives (~120 Hz) but the
 * female TTS voice puts essentially no energy (its fundamental is ~200 Hz+).
 * The reply's echo, however loud, barely registers in this band while the
 * user's voice spikes it. This is the "listen only where the TTS isn't"
 * approach: no waveform subtraction (that's AEC, which never worked on-device),
 * just a frequency region the reply doesn't occupy.
 *
 * It is deliberately dumb — it fires on ANY sustained low-band energy, so a
 * male cough or a clap trips it too. Rejecting those is the caller's job: on a
 * trigger it pauses the reply, transcribes the clip, and only commits the barge
 * if real words come back (a cough transcribes to nothing). See
 * [com.axon.voice.wake.WakeWordService].
 *
 * Not a general solution: it relies on the user's voice sitting BELOW the TTS
 * voice's band. For the current male-user / female-TTS pairing that separation
 * exists; a per-user registered band is future work. The band center is fixed;
 * only the trigger [threshold] is user-tunable (on-device, against the live
 * [level] shown in the notification).
 */
class BandGate(
    sampleRate: Int,
    /** Filtered-band RMS (0..1) a frame must exceed to count as "loud". */
    private val threshold: Double,
    centerHz: Double = 120.0,
    q: Double = 2.0,
    /** Consecutive loud frames required to fire — long enough to reject a
     *  click/clap transient, short enough to catch a real word onset. */
    private val sustainFrames: Int = 4,
) {
    // RBJ cookbook band-pass biquad (constant 0 dB peak gain), coefficients
    // pre-divided by a0 so the difference equation is a plain MAC below.
    private val b0: Double
    private val b2: Double
    private val a1: Double
    private val a2: Double
    private var x1 = 0.0
    private var x2 = 0.0
    private var y1 = 0.0
    private var y2 = 0.0
    private var loud = 0

    /** Last filtered-band RMS seen — surfaced in the notification so the
     *  threshold can be tuned on-device by watching it idle vs. spoken-into. */
    @Volatile
    var level: Double = 0.0
        private set

    init {
        val w0 = 2.0 * PI * centerHz / sampleRate
        val alpha = sin(w0) / (2.0 * q)
        val a0 = 1.0 + alpha
        b0 = alpha / a0
        // b1 is 0 for a band-pass; folded out of the loop.
        b2 = -alpha / a0
        a1 = -2.0 * cos(w0) / a0
        a2 = (1.0 - alpha) / a0
    }

    /** Feed one frame of PCM16 samples. Returns true when a sustained low-band
     *  spike has been seen (a probable barge — confirm with STT). */
    fun process(frame: ShortArray, len: Int = frame.size): Boolean {
        var acc = 0.0
        for (i in 0 until len) {
            val x = frame[i] / 32768.0
            val y = b0 * x + b2 * x2 - a1 * y1 - a2 * y2
            x2 = x1
            x1 = x
            y2 = y1
            y1 = y
            acc += y * y
        }
        val rms = if (len > 0) sqrt(acc / len) else 0.0
        level = rms
        if (rms > threshold) {
            loud++
            if (loud >= sustainFrames) {
                loud = 0
                return true
            }
        } else {
            loud = 0
        }
        return false
    }

    /** Forget the sustained-frame streak — call after a rejected cough so its
     *  own tail doesn't immediately re-fire the gate. */
    fun reset() {
        loud = 0
    }
}
