package com.axon.voice.audio

import android.content.Context
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Persists the user's enrolled voice embedding (a [SpeakerEmbedder.EMBEDDING_DIM]-
 * float vector) as a small private file. One profile per device — no
 * multi-user support, no cloud sync; enrolling again overwrites it.
 */
object VoicePrint {
    private const val FILE_NAME = "voiceprint.bin"

    fun save(context: Context, embedding: FloatArray) {
        val buf = ByteBuffer.allocate(embedding.size * 4).order(ByteOrder.LITTLE_ENDIAN)
        for (v in embedding) buf.putFloat(v)
        File(context.filesDir, FILE_NAME).writeBytes(buf.array())
    }

    /** The enrolled embedding, or null if nothing is enrolled (or the file
     *  is the wrong size — e.g. left over from a different embedding dim). */
    fun load(context: Context): FloatArray? {
        val file = File(context.filesDir, FILE_NAME)
        if (!file.exists()) return null
        val bytes = file.readBytes()
        if (bytes.size != SpeakerEmbedder.EMBEDDING_DIM * 4) return null
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        return FloatArray(SpeakerEmbedder.EMBEDDING_DIM) { buf.float }
    }

    fun exists(context: Context): Boolean = File(context.filesDir, FILE_NAME).exists()

    fun clear(context: Context) {
        File(context.filesDir, FILE_NAME).delete()
    }
}
