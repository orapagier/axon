package com.axon.voice.audio

import com.axon.voice.wake.WakeDetector
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * The mic-side half of barge-in: reads live mic audio while a reply is being
 * spoken and watches for the user interrupting by **saying a keyword** — either
 * the wake word ("Hey Axon") or a short barge word ("okay" / "wait") — each a
 * rustpotter [WakeDetector] run frame-by-frame. A hit on ANY of them confirms.
 *
 * Why keyword spotting and not a loudness/energy threshold: the barge word
 * models are built from the user's own recordings, so detection keys off the
 * *phonetic content of the user's voice*, not how loud the mic is. That is what
 * makes it immune to the failure that sank the energy detector at full speaker
 * volume — the reply's own echo (a different, synthesized voice) doesn't match
 * the user's templates, so it can never self-trigger a barge, and because there
 * is no ducking there is nothing to pump. Measured offline: a 20s reply that
 * literally says "okay"/"wait" scored zero detections against the user-voice
 * model. The trade-off the user accepted: you interrupt by saying the word, not
 * by just talking over the reply.
 *
 * Keeps a rolling pre-roll of raw PCM so a confirmed barge-in doesn't lose
 * whatever the user was already saying before the keyword fired.
 *
 * One instance monitors one reply: construct it when the reply starts speaking,
 * call [run] on its own thread (it blocks), and discard it once it returns.
 * [onConfirmed] fires on that same thread — callers that touch UI marshal to the
 * main thread themselves.
 */
class BargeMonitor(
    /** rustpotter engines to run every frame — the wake word plus any barge
     *  keyword model; a hit on ANY confirms. All must share the same
     *  [WakeDetector.samplesPerFrame] (they do when built with the same
     *  rustpotter config); the caller drops any that don't. */
    private val wakeDetectors: List<WakeDetector>,
    /** Blocking read of exactly one frame's worth of samples; returns false on
     *  a dead stream or a mic-hold — the same contract as
     *  WakeWordService.fillFrame. A plain function so this class carries no
     *  AudioRecord dependency of its own. */
    private val readFrame: (frame: ShortArray) -> Boolean,
    /** How many ~100ms ticks of audio to keep so a confirmed barge-in doesn't
     *  lose the user's first words, spoken before the keyword fired. */
    prerollTicks: Int = PREROLL_TICKS,
    private val onConfirmed: (prerollPcm: ByteArray) -> Unit,
) {
    companion object {
        private const val TICK_SAMPLES = WavRecorder.SAMPLE_RATE / 10 // 100ms @ 16kHz
        const val PREROLL_TICKS = 15 // ~1.5s
    }

    private val frameSize = wakeDetectors.firstOrNull()?.samplesPerFrame ?: TICK_SAMPLES
    private val frame = ShortArray(frameSize)
    private val preroll = ArrayDeque<ByteArray>()
    private val prerollCap = prerollTicks

    private var tickBytes = ByteArrayOutputStream()
    private var tickSamples = 0

    /** Blocks the calling thread until a keyword fires (invoking [onConfirmed]
     *  and returning), [readFrame] returns false (dead mic / mic-hold), or
     *  [isDone] turns true — checked once per iteration so the caller can end
     *  monitoring the instant the reply finishes naturally, racing against the
     *  stream's own completion. */
    fun run(isDone: () -> Boolean) {
        while (!isDone()) {
            if (!readFrame(frame)) return
            appendTick(frame)
            for (d in wakeDetectors) {
                val score = d.process(frame)
                if (score >= 0f) {
                    // TEMP diagnostic at WARN level — HiOS suppresses Log.d
                    // globally (log.tag=I). Shows which keyword fired at what
                    // score, so the barge threshold can be dialed from real
                    // on-device numbers.
                    android.util.Log.w("BargeKeyword", "fired name=${d.name} score=$score")
                    onConfirmed(prerollBytes())
                    return
                }
            }
            if (tickSamples >= TICK_SAMPLES) flushTick()
        }
    }

    private fun appendTick(samples: ShortArray) {
        val bytes = ByteBuffer.allocate(samples.size * 2).order(ByteOrder.LITTLE_ENDIAN)
        for (s in samples) bytes.putShort(s)
        tickBytes.write(bytes.array())
        tickSamples += samples.size
    }

    /** Move the current (possibly partial) tick into the pre-roll ring and
     *  reset the accumulator for the next one. */
    private fun flushTick() {
        preroll.addLast(tickBytes.toByteArray())
        while (preroll.size > prerollCap) preroll.removeFirst()
        tickBytes = ByteArrayOutputStream()
        tickSamples = 0
    }

    /** Raw 16k mono PCM16 bytes of the last ~[prerollCap] ticks, oldest
     *  first — credits whatever was accumulated up to the trigger too, even
     *  if it's short of a full tick. */
    private fun prerollBytes(): ByteArray {
        flushTick()
        val out = ByteArrayOutputStream()
        for (chunk in preroll) out.write(chunk)
        return out.toByteArray()
    }
}
