package com.axon.voice.audio

import ai.onnxruntime.OnnxTensor
import ai.onnxruntime.OrtEnvironment
import ai.onnxruntime.OrtSession
import android.content.Context
import java.nio.FloatBuffer
import kotlin.math.sqrt

/**
 * Wraps the CAM++ speaker-embedding model (assets/campplus.onnx, ~28MB,
 * Apache 2.0, from the 3D-Speaker/CosyVoice ecosystem) via ONNX Runtime
 * Mobile, run directly rather than converted to TFLite: onnx2tf's layout
 * optimizer silently miscomputed this model's dynamic-shape masking layers
 * (verified against the ONNX reference output — cosine similarity ~0.97
 * instead of ~1.0, traced via -cotof to Gather/Shape nodes inside the CAM
 * context-aware-masking layers reading the wrong axis post-conversion).
 * ONNX Runtime executes the original graph, so there's no lossy translation
 * step left to get wrong.
 *
 * [embed] expects 16kHz mono PCM16 (>= one 25ms frame) and returns an
 * L2-normalized 192-dim embedding; compare two embeddings with
 * [cosineSimilarity] — a plain dot product, since both are already
 * unit-length.
 *
 * One instance loads ~28MB of model weights — construct it once (e.g. for
 * the lifetime of an enrollment screen or a barge-in verification pass) and
 * [close] it when done, not per call.
 */
class SpeakerEmbedder(context: Context) : AutoCloseable {
    companion object {
        private const val MODEL_ASSET = "campplus.onnx"
        const val EMBEDDING_DIM = 192
    }

    private val env = OrtEnvironment.getEnvironment()
    private val session: OrtSession = context.assets.open(MODEL_ASSET).use { input ->
        env.createSession(input.readBytes(), OrtSession.SessionOptions())
    }
    private val inputName = session.inputNames.iterator().next()

    /** Runs the model on [pcm16] (16kHz mono) and returns an L2-normalized
     *  192-dim embedding, or null if there's not enough audio for one frame. */
    fun embed(pcm16: ShortArray): FloatArray? {
        val features = SpeakerFeatures.extract(pcm16)
        if (features.isEmpty()) return null

        val numFrames = features.size
        val flat = FloatBuffer.allocate(numFrames * SpeakerFeatures.NUM_MEL_BINS)
        for (frame in features) flat.put(frame)
        flat.rewind()

        val shape = longArrayOf(1, numFrames.toLong(), SpeakerFeatures.NUM_MEL_BINS.toLong())
        OnnxTensor.createTensor(env, flat, shape).use { tensor ->
            session.run(mapOf(inputName to tensor)).use { result ->
                @Suppress("UNCHECKED_CAST")
                val raw = (result[0].value as Array<FloatArray>)[0]
                return l2Normalize(raw)
            }
        }
    }

    private fun l2Normalize(v: FloatArray): FloatArray {
        var sumSq = 0.0
        for (x in v) sumSq += x.toDouble() * x.toDouble()
        val norm = sqrt(sumSq).toFloat()
        if (norm < 1e-9f) return v
        return FloatArray(v.size) { v[it] / norm }
    }

    override fun close() {
        session.close()
    }
}

/** Dot product of two L2-normalized embeddings — their cosine similarity. */
fun cosineSimilarity(a: FloatArray, b: FloatArray): Float {
    var dot = 0f
    for (i in a.indices) dot += a[i] * b[i]
    return dot
}

/** Untuned starting point, not calibrated against real recordings — expect
 *  to retune once this ships. Cosine similarity between two CAM++ embeddings
 *  of the same speaker typically lands well above this; different speakers
 *  well below, but the margin depends on mic/room/threshold conditions this
 *  hasn't been measured against yet. */
const val SPEAKER_SIMILARITY_THRESHOLD = 0.5f

/** Builds a synchronous [BargeMonitor] speaker check from a loaded
 *  [SpeakerEmbedder] and the enrolled voiceprint, or null if either is
 *  missing — [BargeMonitor] treats null as "skip the check", falling back to
 *  the energy-only confirm. */
fun speakerVerifier(
    embedder: SpeakerEmbedder?,
    voiceprint: FloatArray?,
    threshold: Float = SPEAKER_SIMILARITY_THRESHOLD,
): ((ShortArray) -> Boolean)? {
    if (embedder == null || voiceprint == null) return null
    return verifier@{ pcm ->
        val candidate = embedder.embed(pcm) ?: return@verifier false
        cosineSimilarity(candidate, voiceprint) >= threshold
    }
}
