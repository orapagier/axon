package com.axon.voice.audio

import com.axon.voice.wake.WakeDetector
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.sqrt

/**
 * The mic-side half of barge-in: reads live mic audio, feeds a [BargeDetector]
 * on a steady ~100ms RMS cadence, optionally runs the same audio through a
 * rustpotter [WakeDetector] frame-by-frame (the wake word bypasses the
 * echo-vs-speech ambiguity entirely — see [BargeDetector]'s class doc), and
 * keeps a rolling pre-roll of raw PCM so a confirmed barge-in doesn't lose
 * whatever the user was already saying in the ~300-600ms it took to confirm.
 *
 * One instance monitors one reply: construct it when the reply starts
 * speaking, call [run] on its own thread (it blocks), and discard it once it
 * returns. Every callback fires on that same thread — callers that touch UI
 * marshal to the main thread themselves, the same convention every other
 * wake-service callback already follows.
 */
class BargeMonitor(
    private val detector: BargeDetector,
    /** Rustpotter engine to also run frame-by-frame, or null for a mic-only
     *  monitor (push-to-talk's own reply: no wake-word listener is sharing
     *  this mic, so there's nothing to feed). */
    private val wakeDetector: WakeDetector?,
    /** Blocking read of exactly one frame's worth of samples; returns false on
     *  a dead stream or a mic-hold — the same contract as
     *  WakeWordService.fillFrame. A plain function so this class carries no
     *  AudioRecord dependency of its own. */
    private val readFrame: (frame: ShortArray) -> Boolean,
    /** How many ~100ms ticks of audio to keep so a confirmed barge-in doesn't
     *  lose the user's first words, spoken before it actually confirmed. */
    prerollTicks: Int = PREROLL_TICKS,
    private val onTentative: () -> Unit,
    private val onFalseAlarm: () -> Unit,
    private val onConfirmed: (prerollPcm: ByteArray) -> Unit,
    /** Optional "is this actually the enrolled user" check, given the
     *  candidate interruption's buffered preroll PCM — see
     *  [speakerVerifier]. Null when nothing is enrolled (or the embedder
     *  failed to load), which falls back to the energy-only confirm exactly
     *  as before. Runs synchronously on this monitor's own thread right as
     *  an energy-based confirm fires: one ~tens-of-ms embedding inference on
     *  audio already sitting in the preroll buffer, not a per-tick cost, so
     *  it doesn't need its own gate the way [BargeDetector]'s RMS/gain check
     *  does. A rejection is treated exactly like [BargeDetector.Event.FALSE_ALARM]
     *  — volume restores and monitoring continues, since [BargeDetector]'s own
     *  state already reset to idle on the CONFIRMED it just produced. */
    private val verifySpeaker: ((pcm16: ShortArray) -> Boolean)? = null,
) {
    companion object {
        private const val TICK_SAMPLES = WavRecorder.SAMPLE_RATE / 10 // 100ms @ 16kHz
        const val PREROLL_TICKS = 15 // ~1.5s
    }

    private val frame = ShortArray(wakeDetector?.samplesPerFrame ?: TICK_SAMPLES)
    private val preroll = ArrayDeque<ByteArray>()
    private val prerollCap = prerollTicks

    private var tickBytes = ByteArrayOutputStream()
    private var tickSamples = 0
    private var tickSumSq = 0.0

    /** Blocks the calling thread until a confirm fires (invoking [onConfirmed]
     *  and returning), [readFrame] returns false (dead mic / mic-hold), or
     *  [isDone] turns true — checked once per iteration so the caller can end
     *  monitoring the instant the reply finishes naturally, racing against
     *  the stream's own completion. */
    fun run(isDone: () -> Boolean) {
        while (!isDone()) {
            if (!readFrame(frame)) return
            appendTick(frame)
            if (wakeDetector != null && wakeDetector.process(frame) >= 0f) {
                detector.wakeWordHit()
                onConfirmed(prerollBytes())
                return
            }
            if (tickSamples < TICK_SAMPLES) continue
            val rms = sqrt(tickSumSq / tickSamples)
            // Spectral shape of this same window, so BargeDetector can reject a
            // loud broadband burst (cough/clap/pop) that clears the threshold
            // but isn't voiced speech. Computed from the tick's raw PCM before
            // flushTick() resets the accumulator.
            val tickPcm = pcm16(tickBytes.toByteArray())
            val flatness = SpeechShape.flatness(tickPcm)
            val zcr = SpeechShape.zcr(tickPcm)
            flushTick()
            when (detector.feedMic(rms, flatness, zcr)) {
                BargeDetector.Event.TENTATIVE -> onTentative()
                BargeDetector.Event.FALSE_ALARM -> onFalseAlarm()
                BargeDetector.Event.CONFIRMED -> {
                    val preroll = prerollBytes()
                    if (verifySpeaker == null || verifySpeaker.invoke(pcm16(preroll))) {
                        onConfirmed(preroll)
                        return
                    }
                    // Loud and held long enough, but not the enrolled voice —
                    // treat like a false alarm rather than stopping the reply.
                    onFalseAlarm()
                }
                BargeDetector.Event.NONE -> {}
            }
        }
    }

    private fun appendTick(samples: ShortArray) {
        val bytes = ByteBuffer.allocate(samples.size * 2).order(ByteOrder.LITTLE_ENDIAN)
        for (s in samples) {
            bytes.putShort(s)
            val f = s / 32768.0
            tickSumSq += f * f
        }
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
        tickSumSq = 0.0
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

    /** Little-endian PCM16 bytes -> samples, for [verifySpeaker]. */
    private fun pcm16(bytes: ByteArray): ShortArray {
        val buf = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        return ShortArray(bytes.size / 2) { buf.short }
    }
}
