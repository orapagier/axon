package com.axon.androidcompanion.wake

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

    /**
     * Builds a personal wake-word model from a handful of 16k mono PCM16 WAV
     * takes — the same "reference" builder `rustpotter-cli build` runs, just
     * called from the app for on-device enrollment. Returns the serialized
     * `.rpw` bytes, or null if the engine rejects the samples (fewer than 3
     * usable takes, or unreadable audio).
     */
    external fun build(
        name: String,
        samples: Array<ByteArray>,
        threshold: Float,
        avgThreshold: Float,
        mfccSize: Int,
    ): ByteArray?
}

/**
 * On-device wake-word enrollment. Call [build] with 3-8 WAV takes (16k mono
 * PCM16, e.g. from [com.axon.androidcompanion.audio.WavRecorder.wavBytes]) of the phrase
 * the user wants to enroll; the defaults mirror the tuning that shipped with
 * the bundled "Hey Axon" model.
 */
object WakeModelBuilder {
    val available: Boolean get() = RustpotterNative.available

    fun build(
        phrase: String,
        takes: List<ByteArray>,
        threshold: Float = 0.47f,
        avgThreshold: Float = 0.0f,
        mfccSize: Int = 16,
    ): ByteArray? = RustpotterNative.build(phrase, takes.toTypedArray(), threshold, avgThreshold, mfccSize)
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
