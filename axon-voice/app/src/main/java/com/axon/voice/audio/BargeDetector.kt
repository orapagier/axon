package com.axon.voice.audio

/**
 * Duck-then-confirm barge-in detector: decides when the user is trying to
 * interrupt an in-progress spoken reply. The hard part is that the mic hears
 * the reply's own echo bouncing off the room/speaker along with any real
 * speech, so a plain "mic got loud" trigger fires on the assistant's own
 * voice. This separates the two by learning how loud that echo is relative to
 * what's actually playing ([gain]) — per device, per volume, per room, with
 * no calibration step — and requires speech to hold up *after* the reply has
 * been ducked down, where a real interruption keeps going and a stray echo or
 * cough does not.
 *
 * The energy/gain state machine below still matches axon-ui/src/lib/bargein.js
 * line-for-line, but the two platforms now diverge above this class: web
 * layers a cheap spectral speech-shape gate directly into `feedMic` (a
 * browser can't afford much more per 100ms tick), while Android layers a
 * heavier, stronger check — [BargeMonitor]'s `verifySpeaker` runs a CAM++
 * speaker-embedding match ([SpeakerEmbedder]) once, right as an energy-based
 * CONFIRMED fires here, rejecting anyone who isn't the enrolled user
 * ([VoicePrint]) instead of just anyone who isn't speech-shaped. This class
 * itself is unchanged either way — same convention as [SilenceWatcher] /
 * wakeword.js:
 *
 * ```
 *   idle --[mic over threshold]--> tentative (caller ducks the reply)
 *   tentative --[holds MIN_ONSET_TICKS]--> event=CONFIRMED (caller stops + listens)
 *   tentative --[falls back within FALSE_ALARM_TICKS]--> event=FALSE_ALARM (caller restores volume)
 *   any state --[wakeWordHit()]--> event=CONFIRMED
 * ```
 *
 * The wake word always confirms outright rather than going through the
 * tentative window: once the reply is ducked it is no longer fooled by echo
 * (the exact failure a raw rustpotter-on-raw-mic barge monitor hit), and
 * during the silent "thinking" phase there is no playback reference at all
 * for the mic-RMS path to work with, so the wake word is the only signal.
 *
 * [feedMic] must be called on a steady ~100ms cadence — the same cadence
 * [SilenceWatcher] and wakeword.js's `watchSilence` already use — because
 * [MIN_ONSET_TICKS] / [FALSE_ALARM_TICKS] are tick counts, not durations.
 * [feedPlayback] can be called at any rate (Android: ~20ms windows from
 * [PcmPlayback]; web: whatever the sampling loop uses); only the latest value
 * before each [feedMic] tick matters.
 */
class BargeDetector(
    private val absFloor: Double = SilenceWatcher.FOLLOWUP_RMS,
    private val margin: Double = MARGIN,
    private val minOnsetTicks: Int = MIN_ONSET_TICKS,
    private val falseAlarmTicks: Int = FALSE_ALARM_TICKS,
    private val onsetMissGrace: Int = ONSET_MISS_GRACE,
) {
    enum class Event { NONE, TENTATIVE, CONFIRMED, FALSE_ALARM }

    companion object {
        /** How far above the learned echo level the mic must read to count as
         *  real speech rather than the reply bouncing back. */
        const val MARGIN = 2.0

        /** ~300ms at the standard 100ms tick cadence — long enough that a
         *  cough or a door-slam impulse (mirrors SPEECH_ONSET_TICKS elsewhere)
         *  doesn't confirm, but a real interruption always holds this long. */
        const val MIN_ONSET_TICKS = 3

        /** ~600ms — how long a tentative onset is allowed to keep fading
         *  before it's written off as a false alarm and volume is restored. */
        const val FALSE_ALARM_TICKS = 6

        /** At most one isolated non-loud tick in a row is tolerated mid-onset
         *  without resetting progress toward [MIN_ONSET_TICKS] — see
         *  [feedMic]'s miss-streak tracking. Without this cap, a bursty loud
         *  noise (a door slam, dropped object, a cough) spread over several
         *  ticks with gaps under [FALSE_ALARM_TICKS] could accumulate three
         *  non-consecutive loud ticks and confirm on energy alone, invoking
         *  the speaker-embedding check (or, worse, occasionally passing it)
         *  for noise that was never a sustained interruption to begin with. */
        const val ONSET_MISS_GRACE = 1

        /** Slow EMA rate for the learned echo gain: learns over several
         *  seconds of reply audio, not one tick, so one loud consonant can't
         *  swing it and mistrain the threshold. */
        const val GAIN_ALPHA = 0.02

        const val GAIN_MIN = 0.05
        const val GAIN_MAX = 5.0

        /** Default prior before any learning has happened: a phone's own
         *  speaker-into-own-mic echo is typically well below unity gain. */
        const val GAIN_DEFAULT = 0.3

        /** Peak-hold decay applied to the playback reference on every
         *  [feedMic] tick that didn't see a fresh, louder [feedPlayback]
         *  sample — smooths across the ~20ms native sampling gaps and short
         *  inter-sentence pauses so a brief lull mid-reply doesn't read as
         *  the echo suddenly vanishing (which would misfire a confirm). */
        const val PLAYREF_DECAY = 0.85
    }

    private var playRef = 0.0
    private var gain = GAIN_DEFAULT

    private var tentative = false
    private var onsetTicks = 0
    private var quietTicks = 0
    private var missStreak = 0 // consecutive non-loud ticks since the last loud one, while tentative

    /** Feed a playback RMS sample (0..1); a negative value (the convention
     *  [PcmPlayback.onLevel] and the web envelope both use) means "nothing
     *  playing right now" — it never raises the peak-hold, but doesn't reset
     *  it either. [PcmPlayback] emits this between every back-to-back
     *  sentence file, not just at the reply's true end, so a hard reset here
     *  would zero the echo reference at every sentence boundary and let a
     *  room's reverb tail read as a fresh interruption; [PLAYREF_DECAY]
     *  already governs how fast the reference actually falls.
     *
     *  Called from the playback thread while [feedMic] is called from the
     *  barge-monitor thread — every public method here is `@Synchronized` so
     *  the two never race on [playRef]/[gain]. */
    @Synchronized
    fun feedPlayback(rms: Float) {
        val level = if (rms < 0f) 0.0 else rms.toDouble()
        if (level > playRef) playRef = level
    }

    /** Feed one ~100ms mic RMS tick; returns the transition event, if any. */
    @Synchronized
    fun feedMic(rms: Double): Event {
        val threshold = maxOf(absFloor, gain * playRef * margin)
        val event = if (!tentative) {
            if (rms > threshold) {
                tentative = true
                onsetTicks = 1
                quietTicks = 0
                missStreak = 0
                Event.TENTATIVE
            } else {
                if (playRef > absFloor) learnGain(rms)
                Event.NONE
            }
        } else {
            if (rms > threshold) {
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
                // More than one miss in a row: this onset's loud ticks
                // aren't holding together as a sustained interruption, so
                // give up its progress instead of letting a bursty, gappy
                // noise keep accumulating toward minOnsetTicks indefinitely.
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
        // Decay the peak-hold AFTER this tick's threshold/learning used it —
        // the reference for the *next* tick, not this one.
        playRef *= PLAYREF_DECAY
        return event
    }

    /** The wake word fired — always an immediate, unconditional confirm. See
     *  the class doc for why this bypasses the tentative window entirely. */
    @Synchronized
    fun wakeWordHit(): Event {
        tentative = false
        onsetTicks = 0
        quietTicks = 0
        missStreak = 0
        return Event.CONFIRMED
    }

    /** Clears per-turn state (the playback reference, any in-flight
     *  tentative onset) for a fresh reply. Deliberately keeps the learned
     *  [gain] — it took several seconds of real playback to learn and stays
     *  valid across replies on the same device/volume/room. */
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
