package com.axon.voice.audio

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.PI
import kotlin.math.sin

/**
 * Validates [SpeakerFeatures] against a fixture generated from the real
 * Python kaldi_native_fbank reference (see
 * fbank-ref/gen_ref.py in the conversion scratch dir this fixture was copied
 * from) on the exact same deterministic synthetic signal. "Compiles and runs"
 * proves nothing about DSP correctness — this proves the Kotlin pipeline
 * numerically agrees with the training-time feature extractor.
 */
class SpeakerFeaturesTest {

    private fun syntheticSignal(): ShortArray {
        val sr = 16000
        val n = sr // 1.0s
        return ShortArray(n) { i ->
            val t = i.toDouble() / sr
            val sig = 0.3 * sin(2 * PI * 220 * t) +
                0.2 * sin(2 * PI * 880 * t) +
                0.1 * sin(2 * PI * 1500 * t)
            (sig * 32767.0).toInt().coerceIn(-32768, 32767).toShort()
        }
    }

    private fun reference(): JSONObject {
        val stream = javaClass.classLoader!!.getResourceAsStream("fbank_reference.json")!!
        return JSONObject(stream.bufferedReader().readText())
    }

    private fun doubleArray(json: JSONObject, key: String): DoubleArray {
        val arr = json.getJSONArray(key)
        return DoubleArray(arr.length()) { arr.getDouble(it) }
    }

    @Test
    fun `matches the kaldi_native_fbank reference on a synthetic signal`() {
        val ref = reference()
        val pcm = syntheticSignal()
        val out = SpeakerFeatures.extract(pcm)

        assertEquals("num_frames", ref.getInt("num_frames"), out.size)
        assertEquals("num_bins", ref.getInt("num_bins"), out[0].size)

        // Individual bin tolerance: loose enough to absorb FFT/windowing
        // implementation differences between kaldi_native_fbank's C++ and
        // this Kotlin port, tight enough that a real bug (wrong axis, missing
        // preemphasis, wrong window, wrong mel edges) fails loudly — those
        // produce order-of-magnitude or uncorrelated differences, not noise.
        val tol = 0.15

        fun assertFrameClose(label: String, expected: DoubleArray, actual: FloatArray) {
            var maxDiff = 0.0
            for (i in expected.indices) {
                val diff = kotlin.math.abs(expected[i] - actual[i])
                if (diff > maxDiff) maxDiff = diff
            }
            assertTrue("$label max diff $maxDiff exceeds tolerance $tol", maxDiff < tol)
        }

        assertFrameClose("frame_0", doubleArray(ref, "frame_0"), out[0])
        assertFrameClose("frame_50", doubleArray(ref, "frame_50"), out[50])
        assertFrameClose("frame_last", doubleArray(ref, "frame_last"), out[out.size - 1])

        // Per-utterance mean normalization must have actually run: each bin's
        // mean across the clip should be ~0.
        for (m in 0 until out[0].size) {
            var mean = 0.0
            for (f in out) mean += f[m]
            mean /= out.size
            assertTrue("bin $m mean $mean should be ~0 after normalization", kotlin.math.abs(mean) < 0.05)
        }

        val expectedStd = ref.getDouble("overall_std")
        var sumSq = 0.0
        var count = 0
        for (f in out) for (v in f) { sumSq += v.toDouble() * v.toDouble(); count++ }
        val actualStd = kotlin.math.sqrt(sumSq / count)
        assertTrue(
            "overall_std $actualStd should be within 30% of reference $expectedStd",
            kotlin.math.abs(actualStd - expectedStd) < expectedStd * 0.3
        )
    }

    @Test
    fun `returns empty for audio shorter than one frame`() {
        assertEquals(0, SpeakerFeatures.extract(ShortArray(100)).size)
    }

    @Test
    fun `frame count matches the Kaldi snip_edges formula`() {
        // 1 + floor((numSamples - frameLength) / frameShift), frameLength=400, frameShift=160
        val pcm = ShortArray(16000)
        val out = SpeakerFeatures.extract(pcm)
        assertEquals(1 + (16000 - 400) / 160, out.size)
    }
}
