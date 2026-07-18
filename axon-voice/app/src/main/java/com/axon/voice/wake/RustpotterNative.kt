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

class WakeDetector(modelBytes: ByteArray) : AutoCloseable {
    companion object {
        val available: Boolean get() = RustpotterNative.available
    }

    private var handle: Long = RustpotterNative.create(
        modelBytes,
        threshold = 0.47f,
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
