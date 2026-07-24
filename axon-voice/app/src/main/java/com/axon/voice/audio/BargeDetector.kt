package com.axon.voice.audio

/**
 * Duck-then-confirm barge-in detector: decides when the user is trying to
 * interrupt an in-progress spoken reply by talking over it. The hard part is
 * that the mic hears the reply's own echo bouncing off the room/speaker along
 * with any real speech, so a plain "mic got loud" trigger fires on the
 * assistant's own voice. Two mechanisms tell them apart:
 *
 *  1. A **learned echo gain** ([gain]): the ratio of mic level to what's
 *     actually playing is a per-device/room/volume coupling constant, learned
 *     as a slow EMA ([learnGain]) over several seconds of reply audio. The
 *     onset threshold rides at [gain]*playRef*[margin], so ordinary echo
 *     doesn't trip it.
 *  2. **Ducking**: once a loud onset trips, the caller ducks the reply. A real
 *     interruption keeps going (the user's voice doesn't drop when we lower our
 *     own output); the reply's echo drops with it and fades out. So a tentative
 *     onset that keeps holding is real, and one that fades once ducked was echo
 *     — reported as a FALSE_ALARM.
 *
 * On top of the energy test, [feedMic] takes the tick's spectral shape
 * ([SpeechShape]: spectral flatness + zero-crossing rate) so a loud tick only
 * counts toward starting/extending an onset if it's ALSO shaped like voiced
 * speech, not a broadband burst — energy alone can't tell a real interruption
 * from a loud cough, clap, or mic pop, all of which clear the same threshold.
 * The shape gate never touches [learnGain] (which only ever runs on genuinely
 * quiet ticks), so a misclassified cough can't skew the learned echo gain.
 *
 * This is deliberately energy + shape, with no speaker-identity check. An
 * earlier build gated barge-in on a CAM++ speaker-embedding match, but it was
 * effectively impossible to tune (a short interruption clip scores an unreliable
 * similarity against enrollment), so it was removed. The trade-off is that
 * another person's sustained speech can also interrupt — but the assistant's own
 * voice is still rejected by the duck-and-fade test, which is the property that
 * actually matters. Barge-in as a whole is a user toggle
 * ([com.axon.voice.Prefs.bargeInEnabled]); when it's off the mic isn't watched
 * during a reply at all.
 *
 * The state machine mirrors axon-ui/src/lib/bargein.js line-for-line (the two
 * are kept in step, same convention as [SilenceWatcher] / wakeword.js):
 *
 * ```
 *   idle --[mic over threshold AND speech-shaped]--> tentative (caller ducks the reply)
 *   tentative --[holds MIN_ONSET_TICKS]-----------> event=CONFIRMED (caller stops + listens)
 *   tentative --[falls back within FALSE_ALARM_TICKS]--> event=FALSE_ALARM (caller restores volume)
 *   any state --[wakeWordHit()]--------------------> event=CONFIRMED
 * ```
 *
 * The wake word always confirms outright rather than going through the
 * tentative window: once the reply is ducked it is no longer fooled by echo,
 * and during the silent "thinking" phase there is no playback reference at all
 * for the mic-RMS path to work with, so the wake word is the only signal.
 *
 * [feedMic] must be called on a steady ~100ms cadence — the same cadence
 * [SilenceWatcher] and wakeword.js's `watchSilence` already use — because
 * [MIN_ONSET_TICKS] / [FALSE_ALARM_TICKS] are tick counts, not durations.
 * [feedPlayback] can be called at any rate (Android: ~20ms windows from
 * [PcmPlayback]); only the latest value before each [feedMic] tick matters.
 */
class BargeDetector(
    private val absFloor: Double = SilenceWatcher.FOLLOWUP_RMS,
    margin: Double = MARGIN,
    minOnsetTicks: Int = MIN_ONSET_TICKS,
    private val falseAlarmTicks: Int = FALSE_ALARM_TICKS,
    private val onsetMissGrace: Int = ONSET_MISS_GRACE,
    flatnessMax: Double = FLATNESS_MAX,
    zcrMax: Double = ZCR_MAX,
) {
    enum class Event { NONE, TENTATIVE, CONFIRMED, FALSE_ALARM }

    // Runtime-tunable via [tune] (user settings, read fresh each reply). Start
    // at the constructor values, which default to the companion constants.
    private var margin = margin
    private var minOnsetTicks = minOnsetTicks
    private var flatnessMax = flatnessMax
    private var zcrMax = zcrMax

    /** Apply user settings for the upcoming reply. Deliberately does NOT touch
     *  the learned [gain] (that stays valid across replies in the same room) —
     *  callers pair this with [reset], which clears only per-turn state. */
    @Synchronized
    fun tune(margin: Double, minOnsetTicks: Int, speechThreshold: Double) {
        this.margin = margin
        this.minOnsetTicks = minOnsetTicks
        // One user-facing "cough/clap filter" knob drives both shape ceilings.
        this.flatnessMax = speechThreshold
        this.zcrMax = speechThreshold
    }

    /** True when a tick's shape reads as voiced speech rather than a broadband
     *  burst. Requires BOTH features to look speech-like (stricter than either
     *  alone). Thresholds are [flatnessMax]/[zcrMax], tunable via [tune]. */
    private fun looksLikeSpeech(flatness: Double, zcr: Double): Boolean =
        flatness < flatnessMax && zcr < zcrMax

    companion object {
        /** How far above the learned echo level the mic must read to count as a
         *  candidate interruption rather than the reply bouncing back. */
        const val MARGIN = 2.0

        /** ~300ms at the standard 100ms tick cadence — long enough that a cough
         *  or door-slam impulse doesn't confirm, but a real interruption holds
         *  this long. */
        const val MIN_ONSET_TICKS = 3

        /** ~600ms — how long a tentative onset is allowed to keep fading before
         *  it's written off as a false alarm and volume is restored. */
        const val FALSE_ALARM_TICKS = 6

        /** At most one isolated non-qualifying tick in a row is tolerated
         *  mid-onset without resetting progress toward [MIN_ONSET_TICKS] (a
         *  leading fricative inside real speech costs a little delay, not a
         *  dropped confirm). Capping it at one denies a cough's sparse,
         *  mostly-unshaped pattern the same leniency — without the cap, three
         *  stray qualifying ticks scattered across the tentative window could
         *  accumulate to a false CONFIRMED. */
        const val ONSET_MISS_GRACE = 1

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
         *  fire at all (e.g. [PcmPlayback] tearing down and rebuilding a
         *  [android.media.MediaCodec] between two streamed sentences). At 0.85
         *  (~430ms half-life) that gap decayed the reference substantially
         *  before the next sentence's echo arrived, reading ordinary playback as
         *  loud enough to tentatively confirm, duck, then false-alarm back — the
         *  reply's volume pumping at every sentence boundary. 0.94 (~1.1s
         *  half-life) gives the reference enough runway to survive a real pause. */
        const val PLAYREF_DECAY = 0.94

        /** Spectral-flatness ceiling for [looksLikeSpeech]. Voiced speech runs
         *  well under ~0.3 (peaked formant structure); broadband bursts run
         *  flatter, toward 1. Unvalidated against Android's own FFT — calibrate
         *  from [diagnostics] on real cough/speech recordings. */
        const val FLATNESS_MAX = 0.35

        /** Zero-crossing-rate ceiling for [looksLikeSpeech]. Same voiced-vs-burst
         *  split as [FLATNESS_MAX]. Unvalidated — calibrate from [diagnostics].
         *  Default only; the live value is tunable via [tune]. */
        const val ZCR_MAX = 0.35
    }

    private var playRef = 0.0
    private var gain = GAIN_DEFAULT

    private var tentative = false
    private var onsetTicks = 0
    private var quietTicks = 0
    private var missStreak = 0 // consecutive non-qualifying ticks since the last qualifying one

    // Last tick's inputs and the threshold it faced, kept only so the Android
    // call sites can log a one-line "rms=… thr=… gain=… flat=… zcr=…" on each
    // barge event for on-device sanity-checking / calibration. Pure reads.
    private var lastMicRms = 0.0
    private var lastThreshold = 0.0
    private var lastFlatness = 0.0
    private var lastZcr = 0.0
    private var lastSpeechShaped = true

    /** Snapshot of the diagnostics for a one-line logcat entry (see the Android
     *  barge callbacks): last mic RMS, the threshold it faced, the learned echo
     *  gain, and the tick's speech-shape features + verdict. */
    @Synchronized
    fun diagnostics(): String =
        "rms=%.4f thr=%.4f gain=%.3f playRef=%.4f flat=%.3f zcr=%.3f speech=%b".format(
            lastMicRms, lastThreshold, gain, playRef, lastFlatness, lastZcr, lastSpeechShaped
        )

    /** Feed a playback RMS sample (0..1); a negative value (the convention
     *  [PcmPlayback.onLevel] uses) means "nothing playing right now" — it never
     *  raises the peak-hold, but doesn't reset it either. [PcmPlayback] emits
     *  this between every back-to-back sentence file, not just at the reply's
     *  true end, so a hard reset here would zero the echo reference at every
     *  sentence boundary and let a room's reverb tail read as a fresh
     *  interruption; [PLAYREF_DECAY] already governs how fast the reference
     *  actually falls.
     *
     *  Called from the playback thread while [feedMic] is called from the
     *  barge-monitor thread — every public method here is `@Synchronized` so the
     *  two never race on [playRef]/[gain]. */
    @Synchronized
    fun feedPlayback(rms: Float) {
        val level = if (rms < 0f) 0.0 else rms.toDouble()
        if (level > playRef) playRef = level
    }

    /** Feed one ~100ms mic tick: its RMS plus the [SpeechShape] features
     *  ([SpeechShape.flatness] / [SpeechShape.zcr]) of the same audio window.
     *  Returns the transition event, if any. (Android takes the raw features
     *  rather than web's precomputed `speechShaped` boolean so [diagnostics] can
     *  log the actual numbers for calibration — the state-machine logic is
     *  identical.) */
    @Synchronized
    fun feedMic(rms: Double, flatness: Double, zcr: Double): Event {
        val threshold = maxOf(absFloor, gain * playRef * margin)
        val loud = rms > threshold
        val speechShaped = looksLikeSpeech(flatness, zcr)
        val qualifies = loud && speechShaped
        lastMicRms = rms
        lastThreshold = threshold
        lastFlatness = flatness
        lastZcr = zcr
        lastSpeechShaped = speechShaped
        val event = if (!tentative) {
            if (qualifies) {
                tentative = true
                onsetTicks = 1
                quietTicks = 0
                missStreak = 0
                Event.TENTATIVE
            } else {
                // Learn the echo gain only from genuinely-quiet ticks (below
                // threshold) — a loud-but-unshaped tick (a cough) must not reach
                // learnGain just because it failed the shape check, or one bad
                // sample could skew the learned echo gain.
                if (!loud && playRef > absFloor) learnGain(rms)
                Event.NONE
            }
        } else {
            if (qualifies) {
                onsetTicks++
                quietTicks = 0
                missStreak = 0
                if (onsetTicks >= minOnsetTicks) {
                    tentative = false
                    onsetTicks = 0
                    Event.CONFIRMED
                } else {
                    Event.NONE
                }
            } else {
                quietTicks++
                missStreak++
                // More than the tolerated isolated miss in a row: this onset's
                // qualifying ticks aren't holding together as one continuous
                // interruption, so give up its progress rather than let a sparse,
                // mostly-unshaped cough or coughing fit keep accumulating toward
                // minOnsetTicks one stray tick at a time.
                if (missStreak > onsetMissGrace) onsetTicks = 0
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

    /** The wake word fired — always an immediate, unconditional confirm. See the
     *  class doc for why this bypasses the tentative window entirely. */
    @Synchronized
    fun wakeWordHit(): Event {
        tentative = false
        onsetTicks = 0
        quietTicks = 0
        missStreak = 0
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
        missStreak = 0
    }

    private fun learnGain(micRms: Double) {
        val observed = micRms / playRef
        gain = (gain + GAIN_ALPHA * (observed - gain)).coerceIn(GAIN_MIN, GAIN_MAX)
    }
}
