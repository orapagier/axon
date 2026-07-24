package com.axon.voice.audio

import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.ln
import kotlin.math.sin

/**
 * Time/frequency-domain shape of one ~100ms mic tick, for [BargeDetector]'s
 * speech-shape gate. The Android port of axon-ui/src/lib/audioLevel.js's
 * `readZeroCrossingRate` + `readSpectralFlatness`: energy alone can't tell a
 * real interruption from a loud broadband burst (a cough, clap, or mic pop
 * clears the same threshold a real voice does), so [BargeDetector] only lets a
 * loud tick start/extend an onset when its shape also reads as voiced speech.
 *
 * Both measures split voiced speech from bursts for the same underlying reason:
 * voiced speech has a dominant pitch period (few, regular zero crossings) and
 * strong formant structure (a peaked, non-flat spectrum), while an impulsive
 * burst is broadband and aperiodic.
 */
object SpeechShape {
    /** ~-100 dBFS in normalized power; mirrors audioLevel.js excluding bins at
     *  the analyser's noise floor so near-empty bins don't score as "flat". */
    private const val POWER_FLOOR = 1e-10

    /** Zero-crossing rate (0..1): the fraction of adjacent-sample sign flips.
     *  Sustained voiced speech stays low and stable (dominated by the periodic
     *  pitch); coughs/claps/pops cross zero far more often. */
    fun zcr(samples: ShortArray): Double {
        if (samples.size < 2) return 0.0
        var crossings = 0
        for (i in 1 until samples.size) {
            if ((samples[i] >= 0) != (samples[i - 1] >= 0)) crossings++
        }
        return crossings.toDouble() / (samples.size - 1)
    }

    /** Spectral flatness (Wiener entropy, 0..1): geometric mean over arithmetic
     *  mean of the power spectrum. Near 1 = flat/broadband (noise, impulse);
     *  near 0 = peaked/harmonic (voiced speech's formants). Hann-windowed to cut
     *  spectral leakage, DC bin and sub-floor bins excluded. */
    fun flatness(samples: ShortArray): Double {
        val m = samples.size
        if (m < 2) return 0.0
        val n = nextPow2(m)
        val re = DoubleArray(n)
        val im = DoubleArray(n)
        for (i in 0 until m) {
            val w = 0.5 - 0.5 * cos(2.0 * PI * i / (m - 1))
            re[i] = (samples[i] / 32768.0) * w
        }
        fft(re, im)
        var logSum = 0.0
        var sum = 0.0
        var counted = 0
        for (i in 1..n / 2) { // skip DC (bin 0) up to Nyquist
            val power = re[i] * re[i] + im[i] * im[i]
            if (power <= POWER_FLOOR) continue
            logSum += ln(power)
            sum += power
            counted++
        }
        if (counted == 0) return 0.0
        val geoMean = exp(logSum / counted)
        val arithMean = sum / counted
        return if (arithMean > 0.0) geoMean / arithMean else 0.0
    }

    private fun nextPow2(x: Int): Int {
        var p = 1
        while (p < x) p = p shl 1
        return p
    }

    /** In-place iterative radix-2 Cooley-Tukey FFT (size must be a power of 2).
     *  Same routine as [SpeakerFeatures]'s private fft, duplicated here to keep
     *  the barge speech-gate independent of the CAM++ feature pipeline. */
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
