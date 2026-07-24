package com.axon.voice.audio

/**
 * Duck-then-confirm barge-in detector: decides when the user is trying to
 * interrupt an in-progress spoken reply by talking over it. Energy-only — there
 * is no speaker-identity check and no spectral speech-shape gate; an earlier
 * build had both (a CAM++ speaker-embedding "my voice" match, then a spectral
 * cough/clap gate), but both were dropped as impossible to tune reliably and a
 * source of crashes/bloat on-device. What's left is the original, simplest
 * mechanism that actually worked:
 *
 *  1. A **learned echo gain** ([gain]): the mic hears the reply's own echo along
 *     with any real speech, so a plain "mic got loud" trigger fires on the
 *     assistant's own voice. The ratio of mic level to what's actually playing
 *     is a per-device/room/volume coupling constant, learned as a slow EMA
 *     ([learnGain]) over several seconds of reply audio. The onset threshold
 *     rides at [gain]*playRef*[margin], so ordinary echo doesn't trip it.
 *  2. **Ducking**: once a loud onset trips, the caller ducks the reply. A real
 *     interruption keeps going (the user's voice doesn't drop when we lower our
 *     own output); the reply's echo drops with it and fades out. A tentative
 *     onset that keeps holding [MIN_ONSET_TICKS] is real; one that fades within
 *     [FALSE_ALARM_TICKS] was echo (or a brief impulse — a cough/clap doesn't
 *     hold ~300ms), reported as FALSE_ALARM so volume is restored.
 *
 * Barge-in as a whole is a user toggle ([com.axon.voice.Prefs.bargeInEnabled]);
 * when it's off the mic isn't watched during a reply at all. [MARGIN] and
 * [MIN_ONSET_TICKS] are also user-adjustable at runtime via [tune] (read fresh
 * each reply) so sensitivity can be dialed per device/room without a rebuild.
 *
 * State machine (mirrors axon-ui/src/lib/bargein.js's shape):
 *
 * ```
 *   idle --[mic over threshold]--> tentative (caller ducks the reply)
 *   tentative --[holds minOnsetTicks]--> event=CONFIRMED (caller stops + listens)
 *   tentative --[falls back within FALSE_ALARM_TICKS]--> event=FALSE_ALARM (restore volume)
 *   any state --[wakeWordHit()]--> event=CONFIRMED
 * ```
 *
 * The wake word always confirms outright rather than going through the
 * tentative window: once ducked it is no longer fooled by echo, and during the
 * silent "thinking" phase there is no playback reference for the mic-RMS path.
 *
 * [feedMic] must be called on a steady ~100ms cadence — the same cadence
 * [SilenceWatcher] and wakeword.js's `watchSilence` use — because
 * [MIN_ONSET_TICKS] / [FALSE_ALARM_TICKS] are tick counts, not durations.
 * [feedPlayback] can be called at any rate (Android: ~20ms windows from
 * [PcmPlayback]); only the latest value before each [feedMic] tick matters.
 */
class BargeDetector(
    private val absFloor: Double = SilenceWatcher.FOLLOWUP_RMS,
    margin: Double = MARGIN,
    minOnsetTicks: Int = MIN_ONSET_TICKS,
    private val falseAlarmTicks: Int = FALSE_ALARM_TICKS,
) {
    enum class Event { NONE, TENTATIVE, CONFIRMED, FALSE_ALARM }

    companion object {
        /** How far above the learned echo level the mic must read to count as
         *  real speech rather than the reply bouncing back. Default only — the
         *  live value is tunable via [tune]. */
        const val MARGIN = 2.0

        /** ~300ms at the standard 100ms tick cadence — long enough that a cough
         *  or a door-slam impulse doesn't confirm, but a real interruption holds
         *  this long. Default only; tunable via [tune]. */
        const val MIN_ONSET_TICKS = 3

        /** ~600ms — how long a tentative onset is allowed to keep fading before
         *  it's written off as a false alarm and volume is restored. */
        const val FALSE_ALARM_TICKS = 6

        /** Slow EMA rate for the learned echo gain: learns over several seconds
         *  of reply audio, not one tick, so one loud consonant can't swing it
         *  and mistrain the threshold. */
        const val GAIN_ALPHA = 0.02

        const val GAIN_MIN = 0.05
        const val GAIN_MAX = 5.0

        /** Default prior before any learning has happened: a phone's own
         *  speaker-into-own-mic echo is typically well below unity gain. */
        const val GAIN_DEFAULT = 0.3

        /** Peak-hold decay applied once per [feedMic] tick regardless of how
         *  often [feedPlayback] fires — including a genuine gap where it doesn't
         *  fire at all (e.g. [PcmPlayback] rebuilding a codec between two
         *  streamed sentences). 0.94 (~1.1s half-life) gives the reference enough
         *  runway to survive a real inter-sentence pause without reading ordinary
         *  continuing playback as a fresh interruption (which pumped the volume). */
        const val PLAYREF_DECAY = 0.94
    }

    // Runtime-tunable via [tune] (user settings, read fresh each reply). Start
    // at the constructor values, which default to the companion constants.
    private var margin = margin
    private var minOnsetTicks = minOnsetTicks

    private var playRef = 0.0
    private var gain = GAIN_DEFAULT

    private var tentative = false
    private var onsetTicks = 0
    private var quietTicks = 0

    // Last tick's mic RMS and the threshold it faced, kept only so the Android
    // call sites can log "rms=… thr=… gain=…" on each barge event for on-device
    // sanity-checking. Pure reads.
    private var lastMicRms = 0.0
    private var lastThreshold = 0.0

    /** Snapshot of the diagnostics for a one-line logcat entry. */
    @Synchronized
    fun diagnostics(): String =
        "rms=%.4f thr=%.4f gain=%.3f playRef=%.4f".format(lastMicRms, lastThreshold, gain, playRef)

    /** Apply user settings for the upcoming reply. Deliberately does NOT touch
     *  the learned [gain] (that stays valid across replies in the same room) —
     *  callers pair this with [reset], which clears only per-turn state. */
    @Synchronized
    fun tune(margin: Double, minOnsetTicks: Int) {
        this.margin = margin
        this.minOnsetTicks = minOnsetTicks
    }

    /** Feed a playback RMS sample (0..1); a negative value (the convention
     *  [PcmPlayback.onLevel] uses) means "nothing playing right now" — it never
     *  raises the peak-hold, but doesn't reset it either.
     *
     *  Called from the playback thread while [feedMic] is called from the
     *  barge-monitor thread — every public method here is `@Synchronized`. */
    @Synchronized
    fun feedPlayback(rms: Float) {
        val level = if (rms < 0f) 0.0 else rms.toDouble()
        if (level > playRef) playRef = level
    }

    /** Feed one ~100ms mic RMS tick; returns the transition event, if any. */
    @Synchronized
    fun feedMic(rms: Double): Event {
        val threshold = maxOf(absFloor, gain * playRef * margin)
        lastMicRms = rms
        lastThreshold = threshold
        val event = if (!tentative) {
            if (rms > threshold) {
                tentative = true
                onsetTicks = 1
                quietTicks = 0
                Event.TENTATIVE
            } else {
                if (playRef > absFloor) learnGain(rms)
                Event.NONE
            }
        } else {
            if (rms > threshold) {
                onsetTicks++
                quietTicks = 0
                if (onsetTicks >= minOnsetTicks) {
                    tentative = false
                    onsetTicks = 0
                    Event.CONFIRMED
                } else {
                    Event.NONE
                }
            } else {
                quietTicks++
                if (quietTicks >= falseAlarmTicks) {
                    tentative = false
                    onsetTicks = 0
                    quietTicks = 0
                    Event.FALSE_ALARM
                } else {
                    Event.NONE
                }
            }
        }
        // Decay the peak-hold AFTER this tick's threshold/learning used it — the
        // reference for the *next* tick, not this one.
        playRef *= PLAYREF_DECAY
        return event
    }

    /** The wake word fired — always an immediate, unconditional confirm. */
    @Synchronized
    fun wakeWordHit(): Event {
        tentative = false
        onsetTicks = 0
        quietTicks = 0
        return Event.CONFIRMED
    }

    /** Clears per-turn state (the playback reference, any in-flight tentative
     *  onset) for a fresh reply. Deliberately keeps the learned [gain] — it took
     *  several seconds of real playback to learn and stays valid across replies
     *  on the same device/volume/room. */
    @Synchronized
    fun reset() {
        playRef = 0.0
        tentative = false
        onsetTicks = 0
        quietTicks = 0
    }

    private fun learnGain(micRms: Double) {
        val observed = micRms / playRef
        gain = (gain + GAIN_ALPHA * (observed - gain)).coerceIn(GAIN_MIN, GAIN_MAX)
    }
}
