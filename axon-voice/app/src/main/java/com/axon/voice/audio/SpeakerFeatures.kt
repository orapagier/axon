package com.axon.voice.audio

import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.ln
import kotlin.math.max
import kotlin.math.pow
import kotlin.math.sin

/**
 * 80-bin log-mel filterbank ("fbank") features, computed to match the
 * Kaldi/kaldi_native_fbank convention CAM++ (the on-device speaker-embedding
 * model in [SpeakerEmbedder]) was trained on: 25ms/10ms frames, no dither,
 * DC removal then 0.97 preemphasis then a Povey window, a 512-point FFT,
 * Kaldi's own mel scale (not HTK's), and per-utterance mean normalization
 * (subtract each bin's mean across the whole clip — no variance scaling).
 *
 * Pure math, no Android dependencies, so it's unit-testable on the JVM
 * against a numeric fixture generated from the real kaldi_native_fbank
 * Python reference (see SpeakerFeaturesTest) — the DSP has to match the
 * training-time pipeline closely or the embedding degrades silently, so
 * "compiles" is not enough evidence this is right.
 */
object SpeakerFeatures {
    const val SAMPLE_RATE = 16000
    const val NUM_MEL_BINS = 80

    private const val FRAME_LENGTH_MS = 25
    private const val FRAME_SHIFT_MS = 10
    private const val PREEMPH_COEFF = 0.97
    private const val LOW_FREQ = 20.0
    private const val LOG_FLOOR = 1.1920929e-7 // ~FLT_EPSILON, mirrors Kaldi's mel-energy floor

    private val FRAME_LENGTH = SAMPLE_RATE * FRAME_LENGTH_MS / 1000 // 400
    private val FRAME_SHIFT = SAMPLE_RATE * FRAME_SHIFT_MS / 1000 // 160
    private val PADDED_SIZE = nextPowerOfTwo(FRAME_LENGTH) // 512
    private val NUM_FFT_BINS = PADDED_SIZE / 2 // 256 — Kaldi excludes the Nyquist bin from the mel banks

    private val window = poveyWindow(FRAME_LENGTH)
    private val melBank = melFilterBank()

    /** [numFrames][NUM_MEL_BINS] log-mel features, mean-normalized per bin
     *  across the whole clip. Empty if [pcm16] is shorter than one frame. */
    fun extract(pcm16: ShortArray): Array<FloatArray> {
        val numFrames = if (pcm16.size < FRAME_LENGTH) 0 else 1 + (pcm16.size - FRAME_LENGTH) / FRAME_SHIFT
        if (numFrames <= 0) return emptyArray()

        val out = Array(numFrames) { DoubleArray(NUM_MEL_BINS) }
        val real = DoubleArray(PADDED_SIZE)
        val imag = DoubleArray(PADDED_SIZE)
        val frame = DoubleArray(FRAME_LENGTH)

        for (f in 0 until numFrames) {
            val start = f * FRAME_SHIFT
            for (i in 0 until FRAME_LENGTH) frame[i] = pcm16[start + i].toDouble()

            // Remove DC offset, then preemphasis, then window — Kaldi's order.
            val mean = frame.sum() / FRAME_LENGTH
            for (i in 0 until FRAME_LENGTH) frame[i] -= mean
            for (i in FRAME_LENGTH - 1 downTo 1) frame[i] -= PREEMPH_COEFF * frame[i - 1]
            frame[0] -= PREEMPH_COEFF * frame[0]
            for (i in 0 until FRAME_LENGTH) frame[i] *= window[i]

            real.fill(0.0)
            imag.fill(0.0)
            for (i in 0 until FRAME_LENGTH) real[i] = frame[i]
            fft(real, imag)

            for (m in 0 until NUM_MEL_BINS) {
                val w = melBank[m]
                var energy = 0.0
                for (i in 0 until NUM_FFT_BINS) {
                    if (w[i] != 0.0) energy += w[i] * (real[i] * real[i] + imag[i] * imag[i])
                }
                out[f][m] = ln(max(energy, LOG_FLOOR.toDouble()))
            }
        }

        // Per-utterance mean normalization: subtract each bin's mean across
        // every frame in the clip. Mean-only (no variance scaling) — matches
        // the reference pipeline CAM++'s training/inference scripts use.
        val meanPerBin = DoubleArray(NUM_MEL_BINS)
        for (frameVals in out) for (m in 0 until NUM_MEL_BINS) meanPerBin[m] += frameVals[m]
        for (m in 0 until NUM_MEL_BINS) meanPerBin[m] /= numFrames

        return Array(numFrames) { f ->
            FloatArray(NUM_MEL_BINS) { m -> (out[f][m] - meanPerBin[m]).toFloat() }
        }
    }

    private fun poveyWindow(n: Int): DoubleArray {
        val a = 2.0 * PI / (n - 1)
        return DoubleArray(n) { i -> (0.5 - 0.5 * cos(a * i)).pow(0.85) }
    }

    private fun melScale(freqHz: Double): Double = 1127.0 * ln(1.0 + freqHz / 700.0)

    /** [NUM_MEL_BINS] triangular filters over [NUM_FFT_BINS] power-spectrum
     *  bins, spaced evenly on Kaldi's mel scale from [LOW_FREQ] to Nyquist. */
    private fun melFilterBank(): Array<DoubleArray> {
        val nyquist = SAMPLE_RATE / 2.0
        val fftBinWidth = SAMPLE_RATE.toDouble() / PADDED_SIZE
        val melLow = melScale(LOW_FREQ)
        val melHigh = melScale(nyquist)
        val melDelta = (melHigh - melLow) / (NUM_MEL_BINS + 1)

        return Array(NUM_MEL_BINS) { bin ->
            val leftMel = melLow + bin * melDelta
            val centerMel = melLow + (bin + 1) * melDelta
            val rightMel = melLow + (bin + 2) * melDelta
            DoubleArray(NUM_FFT_BINS) { i ->
                val mel = melScale(fftBinWidth * i)
                when {
                    mel <= leftMel || mel >= rightMel -> 0.0
                    mel <= centerMel -> (mel - leftMel) / (centerMel - leftMel)
                    else -> (rightMel - mel) / (rightMel - centerMel)
                }
            }
        }
    }

    private fun nextPowerOfTwo(n: Int): Int {
        var p = 1
        while (p < n) p = p shl 1
        return p
    }

    /** In-place iterative radix-2 Cooley-Tukey FFT (size must be a power of 2). */
    private fun fft(real: DoubleArray, imag: DoubleArray) {
        val n = real.size
        var j = 0
        for (i in 1 until n) {
            var bit = n shr 1
            while (j and bit != 0) {
                j = j xor bit
                bit = bit shr 1
            }
            j = j xor bit
            if (i < j) {
                var t = real[i]; real[i] = real[j]; real[j] = t
                t = imag[i]; imag[i] = imag[j]; imag[j] = t
            }
        }
        var len = 2
        while (len <= n) {
            val ang = -2.0 * PI / len
            val wr = cos(ang)
            val wi = sin(ang)
            var i = 0
            while (i < n) {
                var curWr = 1.0
                var curWi = 0.0
                for (k in 0 until len / 2) {
                    val ur = real[i + k]
                    val ui = imag[i + k]
                    val vr = real[i + k + len / 2] * curWr - imag[i + k + len / 2] * curWi
                    val vi = real[i + k + len / 2] * curWi + imag[i + k + len / 2] * curWr
                    real[i + k] = ur + vr
                    imag[i + k] = ui + vi
                    real[i + k + len / 2] = ur - vr
                    imag[i + k + len / 2] = ui - vi
                    val nwr = curWr * wr - curWi * wi
                    val nwi = curWr * wi + curWi * wr
                    curWr = nwr
                    curWi = nwi
                }
                i += len
            }
            len = len shl 1
        }
    }
}
