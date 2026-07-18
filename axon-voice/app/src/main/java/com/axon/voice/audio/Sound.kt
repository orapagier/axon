package com.axon.voice.audio

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import kotlin.concurrent.thread
import kotlin.math.PI
import kotlin.math.exp
import kotlin.math.ln
import kotlin.math.sin

/**
 * The dashboard's "I'm listening" cues, synthesized instead of shipped as
 * assets: two rising sine notes (660->880Hz, ~0.3s) for the wake chime, one
 * quieter 880Hz note for the follow-up window cue.
 */
object Sound {
    private const val SR = 22050

    fun chime(soft: Boolean = false) {
        thread(name = "axon-chime") {
            val dur = if (soft) 0.18 else 0.3
            val peak = if (soft) 0.08 else 0.15
            val attack = 0.02
            val n = (SR * (dur + 0.02)).toInt()
            val buf = ShortArray(n)
            // exponentialRampToValueAtTime(0.0001, dur) equivalent decay rate
            val decayK = ln(0.0001 / peak) / (dur - attack)
            for (i in 0 until n) {
                val t = i.toDouble() / SR
                val f = if (soft) 880.0 else if (t < 0.1) 660.0 else 880.0
                val env = when {
                    t < attack -> peak * (t / attack)
                    t < dur -> peak * exp(decayK * (t - attack))
                    else -> 0.0
                }
                buf[i] = (sin(2 * PI * f * t) * env * 32767).toInt().toShort()
            }
            val track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_ASSISTANT)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build()
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setSampleRate(SR)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build()
                )
                .setTransferMode(AudioTrack.MODE_STATIC)
                .setBufferSizeInBytes(n * 2)
                .build()
            try {
                track.write(buf, 0, n)
                track.play()
                Thread.sleep(((dur + 0.1) * 1000).toLong())
            } catch (_: Exception) {
                // no audio out — state change is still visible in the UI
            } finally {
                track.release()
            }
        }
    }
}
