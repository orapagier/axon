package com.axon.voice.audio

/**
 * Duck-then-confirm barge-in detector: decides when the user is trying to
 * interrupt an in-progress spoken reply by talking over it. The hard part is
 * that the mic hears the reply's own echo bouncing off the room/speaker along
 * with any real speech, so a plain "mic got loud" trigger fires on the
 * assistant's own voice. Two mechanisms tell them apart, and neither needs the
 * user to tune anything:
 *
 *  1. A **self-calibrating echo estimate** ([gain]): the ratio of mic level to
 *     what's actually playing is a per-device/room/volume coupling constant, so
 *     [learnGain] tracks it as a peak-hold with slow decay — it sits at (or just
 *     above) the loudest recent echo ratio and can't chronically *under*estimate
 *     the way a plain average could. An underestimate was the old failure mode:
 *     it let ordinary echo cross the threshold, duck, fall back, and duck again
 *     — the reply's volume audibly "pumping." A peak-hold estimate makes the
 *     threshold (`gain*playRef*margin`) ride above the echo, so ordinary
 *     playback never trips it in the first place.
 *  2. **Ducking**: once a loud onset does trip, the caller ducks the reply. A
 *     real interruption keeps going (the user's voice doesn't drop when we lower
 *     our own output); the reply's echo drops with it and fades out. So a
 *     tentative onset that keeps holding is real, and one that fades once ducked
 *     was echo — reported as a FALSE_ALARM, after which a short cooldown blocks
 *     an immediate re-duck so a single loud transient can't start a pumping bout.
 *
 * This is deliberately energy-only: there is no speaker-identity check. An
 * earlier build gated barge-in on a CAM++ speaker-embedding match, but it was
 * effectively impossible to tune (a short interruption clip scores an unreliable
 * similarity against enrollment), so it was removed. The trade-off is that
 * another person's sustained speech can also interrupt — but the assistant's own
 * voice is still rejected by the duck-and-fade test above, which is the property
 * that actually matters. Barge-in as a whole is a user toggle
 * ([com.axon.voice.Prefs.bargeInEnabled]); when it's off the mic isn't watched
 * during a reply at all.
 *
 * The state machine has the same shape as axon-ui/src/lib/bargein.js (which
 * still uses a plain learned gain + a spectral speech-shape gate — the two
 * platforms diverged and only Android self-calibrates this way):
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
 * [PcmPlayback]); only the latest value before each [feedMic] tick matters.
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
        /** How far above the estimated echo level the mic must read to *start*
         *  ducking (the tentative onset). This only has to clear ordinary echo
         *  well enough to begin the duck-and-fade test — that test, not this
         *  margin, is what actually rejects echo: a real interruption keeps
         *  holding once ducked while echo fades. The mic runs with no AEC
         *  ([WakeWordService.openRecord]), and a phone's own speaker sits closer
         *  to its mic than the user does, so at 2.0 a normal-volume talk-over
         *  often couldn't clear 2x the full-volume echo and never engaged the
         *  duck at all. 1.4 keeps the peak-hold [gain] riding above steady echo
         *  (which sits below its own recent peak) while letting an ordinary
         *  interruption trip the first duck; false onsets from echo peaks still
         *  fade out and are caught as [Event.FALSE_ALARM], bounded to a brief dip
         *  by [FALSE_ALARM_COOLDOWN_TICKS]. Fixed, not tunable — the
         *  room-dependent part is [gain], which self-calibrates. */
        const val MARGIN = 1.4

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
         *  non-consecutive loud ticks and confirm on energy alone for noise
         *  that was never a sustained interruption to begin with. */
        const val ONSET_MISS_GRACE = 1

        /** After a FALSE_ALARM, how many ticks to refuse to start a new
         *  tentative onset (~500ms). The duck-and-fade test already proved that
         *  onset was echo, not speech; this short refractory window means a
         *  single loud echo spike (or a stray transient) can't immediately
         *  re-duck and start the volume oscillating. Learning still runs during
         *  the cooldown, so [gain] keeps catching up to the echo it just saw and
         *  the *next* onset check faces a correctly-raised threshold. Bounds the
         *  worst case to one brief dip per ~1.1s — not perceptible pumping — and
         *  in practice the peak-hold [gain] prevents even that after one cycle. */
        const val FALSE_ALARM_COOLDOWN_TICKS = 5

        const val GAIN_MIN = 0.05
        const val GAIN_MAX = 2.5

        /** Per-quiet-tick decay of the peak-held echo [gain]. The estimate jumps
         *  up instantly to any louder echo ratio and relaxes back down at this
         *  rate (~0.99/tick ≈ 7s half-life), so a one-off loud consonant lifts
         *  the threshold only briefly instead of desensitizing barge-in for the
         *  rest of the reply, while a steady echo ratio holds the estimate right
         *  at its own level. */
        const val GAIN_DECAY = 0.99

        /** Default prior before any learning has happened: a phone's own
         *  speaker-into-own-mic echo is typically well below unity gain. */
        const val GAIN_DEFAULT = 0.3

        /** Peak-hold decay applied once per [feedMic] tick regardless of how
         *  often [feedPlayback] fires — including a genuine gap where it
         *  doesn't fire at all, e.g. [PcmPlayback] tearing down and building a
         *  fresh [android.media.MediaCodec] between two streamed sentences.
         *  At 0.85 (~430ms half-life), that gap decayed the reference
         *  substantially before the next sentence's own echo arrived, so the
         *  threshold read ordinary continuing playback as loud enough to
         *  tentatively confirm, duck, then false-alarm back: the reply's volume
         *  pumping at every sentence boundary. 0.94 (~1.1s half-life) gives the
         *  reference enough runway to survive a real pause without depending on
         *  decode/codec timing. */
        const val PLAYREF_DECAY = 0.94
    }

    private var playRef = 0.0
    private var gain = GAIN_DEFAULT

    private var tentative = false
    private var onsetTicks = 0
    private var quietTicks = 0
    private var missStreak = 0 // consecutive non-loud ticks since the last loud one, while tentative
    private var cooldownTicks = 0 // ticks left refusing a new onset after a false alarm

    // Last tick's mic RMS and the threshold it was compared against, kept only
    // so the Android call sites can log "rms=… thr=… gain=…" on each barge
    // event for on-device sanity-checking. Pure reads; kept off the companion
    // so a JVM test can still exercise this class with no android.util.Log dep.
    private var lastMicRms = 0.0
    private var lastThreshold = 0.0

    /** Snapshot of the diagnostics for a one-line logcat entry (see the
     *  Android barge callbacks): last mic RMS, the threshold it faced, and the
     *  current self-calibrated echo gain. */
    @Synchronized
    fun diagnostics(): String =
        "rms=%.4f thr=%.4f gain=%.3f playRef=%.4f".format(
            lastMicRms, lastThreshold, gain, playRef
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
        lastMicRms = rms
        lastThreshold = threshold
        val event = if (!tentative) {
            when {
                cooldownTicks > 0 -> {
                    // Refractory window after a false alarm: don't start a new
                    // onset, but keep calibrating so gain catches the echo up.
                    cooldownTicks--
                    if (playRef > absFloor) learnGain(rms)
                    Event.NONE
                }
                rms > threshold -> {
                    tentative = true
                    onsetTicks = 1
                    quietTicks = 0
                    missStreak = 0
                    Event.TENTATIVE
                }
                else -> {
                    if (playRef > absFloor) learnGain(rms)
                    Event.NONE
                }
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
                    // The loud onset that ducked us faded once ducked — echo,
                    // not a real interruption. Hold off starting another onset
                    // for a moment so we can't immediately re-duck on the same
                    // echo and pump the volume.
                    cooldownTicks = FALSE_ALARM_COOLDOWN_TICKS
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
        cooldownTicks = 0
        return Event.CONFIRMED
    }

    /** Clears per-turn state (the playback reference, any in-flight tentative
     *  onset, any cooldown) for a fresh reply. Deliberately keeps the learned
     *  [gain] — it took several seconds of real playback to calibrate and stays
     *  valid across replies on the same device/volume/room, so a second reply in
     *  the same room doesn't dip at all while gain re-converges. */
    @Synchronized
    fun reset() {
        playRef = 0.0
        tentative = false
        onsetTicks = 0
        quietTicks = 0
        missStreak = 0
        cooldownTicks = 0
    }

    /** Peak-hold-with-decay of the observed echo ratio. Jumps up to any louder
     *  ratio immediately, decays toward it at [GAIN_DECAY] otherwise — so the
     *  estimate sits at the top of the recent echo envelope and never chronically
     *  underestimates (which is what let echo keep tripping the threshold and
     *  pump the volume). The ratio is level-independent (mic echo and [playRef]
     *  both scale with playback volume), so it settles cleanly at the room's
     *  coupling constant regardless of how loud the current sentence is. */
    private fun learnGain(micRms: Double) {
        val observed = micRms / playRef
        gain = maxOf(gain * GAIN_DECAY, observed).coerceIn(GAIN_MIN, GAIN_MAX)
    }
}
