package com.axon.voice.wake

/**
 * JNI bridge to the rustpotter wake-word engine (rustpotter-jni crate, built
 * via cargo-ndk into jniLibs). Detector settings mirror the dashboard's
 * axon-ui/src/lib/wakeword.js, which mirrors the CLI flags that passed the
 * live mic test: spot -g -e -t 0.47.
 */
internal object RustpotterNative {
    val available: Boolean = try {
        System.loadLibrary("rustpotter_jni")
        true
    } catch (_: Throwable) {
        false
    }

    external fun create(
        model: ByteArray,
        threshold: Float,
        avgThreshold: Float,
        scoreRef: Float,
        minScores: Int,
        eager: Boolean,
        gainNormalizer: Boolean,
    ): Long

    external fun samplesPerFrame(handle: Long): Int

    /** Feed one frame of 16k mono PCM16; returns the detection score, or -1 when nothing fired. */
    external fun process(handle: Long, samples: ShortArray): Float

    external fun destroy(handle: Long)
}

/**
 * @param threshold single-frame detection cutoff. Default 0.47 is the "Hey Axon"
 *   value.
 * @param avgThreshold averaged-score gate: the detection window's mean score
 *   must also clear this. 0.0 (off) for the far-field wake word.
 * @param name label for diagnostics only (which detector fired), not used by
 *   the engine.
 */
class WakeDetector(
    modelBytes: ByteArray,
    threshold: Float = 0.47f,
    avgThreshold: Float = 0.0f,
    val name: String = "wake",
) : AutoCloseable {
    companion object {
        val available: Boolean get() = RustpotterNative.available
    }

    private var handle: Long = RustpotterNative.create(
        modelBytes,
        threshold = threshold,
        avgThreshold = avgThreshold,
        scoreRef = 0.22f,
        minScores = 10,
        eager = true,
        gainNormalizer = true,
    )

    init {
        check(handle != 0L) { "wake word model rejected" }
    }

    val samplesPerFrame: Int get() = RustpotterNative.samplesPerFrame(handle)

    fun process(samples: ShortArray): Float = RustpotterNative.process(handle, samples)

    override fun close() {
        if (handle != 0L) {
            RustpotterNative.destroy(handle)
            handle = 0
        }
    }
}
