package com.axon.voice.audio

/**
 * Duck-then-confirm barge-in detector: decides when the user is trying to
 * interrupt an in-progress spoken reply by talking over it. Energy-only — there
 * is no speaker-identity check and no spectral speech-shape gate; an earlier
 * build had both (a CAM++ speaker-embedding "my voice" match, then a spectral
 * cough/clap gate), but both were dropped as impossible to tune reliably and a
 * source of crashes/bloat on-device.
 *
 * The hard problem is telling the user's voice from the reply's own echo
 * bouncing back into the mic. The reference the detector has for "what's
 * playing" ([playRef], fed from [PcmPlayback.onLevel]) is the RMS of the decoded
 * PCM **source** — it does NOT include the device's master-volume scaling, so at
 * full volume the sound actually leaving the speaker (and the echo returning to
 * the mic) is far louder than [playRef]. A plain "mic louder than playRef ×
 * threshold" test therefore breaks at high volume: the echo dwarfs the
 * reference, clears the bar, and — if it stays above it long enough — CONFIRMS.
 * That captured the reply's own voice as the user's turn, sent it back, and
 * looped the assistant onto itself. A slowly-learned echo gain can't save it,
 * because at full volume the echo confirms *before* the gain climbs, and a
 * confirm ends the turn so the gain never learns.
 *
 * The fix is a **ducked-residual test** — the one discriminator that's robust to
 * however loud the echo is:
 *
 *  1. **Onset**: mic RMS over [gain]*[playRef]*[margin] starts a *tentative*
 *     onset; the caller ducks the reply to [duckVolume]. This is just a trigger
 *     to run the real test — deliberately eager (better to duck-and-test than
 *     miss a real interruption).
 *  2. **Settle**: for [SETTLE_TICKS] the just-issued duck is still working its
 *     way through the ~100ms playback buffer to the DAC, so the echo is still at
 *     full volume. These ticks are NOT judged — judging here is exactly what let
 *     full-volume echo confirm. They're used only to sharpen [tripRatio], the
 *     clean coupling estimate (mic/playRef) the residual test compares against.
 *  3. **Residual test**: once ducked, *pure echo* drops to ~[duckVolume] of what
 *     it was — its mic/playRef ratio collapses to ~[tripRatio]*[duckVolume],
 *     independent of the reply's moment-to-moment loudness (numerator and
 *     coupling scale together). A real voice does NOT drop when we duck. So:
 *       - ratio still well above the ducked-echo line (× [DUCK_RESIDUAL_MARGIN])
 *         for [minOnsetTicks] running ticks → a real voice is carrying it →
 *         CONFIRMED (caller stops + listens).
 *       - ratio collapses onto the ducked-echo line for [DUCKED_ECHO_TICKS]
 *         running ticks → it was our echo → FALSE_ALARM. The clean [tripRatio]
 *         is folded into [gain] fast ([FALSE_ALARM_GAIN_ALPHA]) so the onset bar
 *         rises above this coupling and stops re-ducking — which is what stops
 *         the reply volume pumping up and down.
 *       - neither resolves within [falseAlarmTicks] → default to FALSE_ALARM.
 *         Ambiguity never confirms, so it can never loop onto its own voice.
 *
 * Because pure echo can't clear the residual line no matter how loud it is, the
 * loop is structurally impossible now; the price is that a real interruption
 * must be at least [DUCK_RESIDUAL_MARGIN]× the ducked residual to be heard,
 * which is roughly "louder than the reply is once ducked" — the point past which
 * it's audible to the user anyway.
 *
 * The wake word bypasses all of this ([wakeWordHit]) — an immediate confirm,
 * since it's matched on speech content, not loudness.
 *
 * Barge-in as a whole is a user toggle ([com.axon.voice.Prefs.bargeInEnabled]);
 * when it's off the mic isn't watched during a reply at all. [MARGIN] and
 * [MIN_ONSET_TICKS] are user-adjustable at runtime via [tune] (read fresh each
 * reply) so onset sensitivity can be dialed per device/room without a rebuild.
 *
 * [feedMic] must be called on a steady ~100ms cadence — the same cadence
 * [SilenceWatcher] and wakeword.js's `watchSilence` use — because the tick
 * counts ([minOnsetTicks], [SETTLE_TICKS], …) are counts, not durations.
 * [feedPlayback] can be called at any rate (Android: ~20ms windows from
 * [PcmPlayback]); only the latest value before each [feedMic] tick matters.
 */
class BargeDetector(
    private val absFloor: Double = SilenceWatcher.FOLLOWUP_RMS,
    margin: Double = MARGIN,
    minOnsetTicks: Int = MIN_ONSET_TICKS,
    private val falseAlarmTicks: Int = FALSE_ALARM_TICKS,
    /** Attenuation the caller applies to the reply on a tentative onset — the
     *  factor pure echo's mic level drops by, which the residual test keys off.
     *  Defaults to what [TtsPlayer.duck] actually applies. */
    private val duckVolume: Double = TtsPlayer.DUCK_VOLUME.toDouble(),
) {
    enum class Event { NONE, TENTATIVE, CONFIRMED, FALSE_ALARM }

    companion object {
        /** How far above the learned echo level the mic must read to START
         *  ducking (a tentative onset). Default only — tunable via [tune]. This
         *  is just the trigger to run the ducked-residual test; the test itself
         *  is what actually tells echo from speech. */
        const val MARGIN = 2.0

        /** Running ducked ticks a real voice must hold above the residual line to
         *  confirm — ~300ms at the 100ms tick cadence. Default only; tunable via
         *  [tune]. */
        const val MIN_ONSET_TICKS = 3

        /** How many ticks the just-issued duck is still draining out of the
         *  ~100ms playback buffer, during which the echo is still full-volume and
         *  must NOT be judged (judging here is what let loud echo confirm). ~2
         *  ticks ≈ 200ms covers the buffer floor plus the acoustic round-trip. */
        const val SETTLE_TICKS = 2

        /** Running ducked ticks whose mic level collapsed onto the ducked-echo
         *  line before the onset is written off as echo (a FALSE_ALARM). Short so
         *  echo is dismissed fast — ~200ms — which is what keeps the reply from
         *  audibly pumping; a genuine interruption holds continuously and reaches
         *  [minOnsetTicks] first, so it confirms before this ever trips. */
        const val DUCKED_ECHO_TICKS = 2

        /** A ducked tick counts as a real voice only if its mic/playRef ratio is
         *  at least this multiple of the expected pure-echo residual
         *  ([tripRatio]*[duckVolume]). Pure echo sits at ~1× that; a talking user
         *  rides well above it. */
        const val DUCK_RESIDUAL_MARGIN = 1.8

        /** ~600ms — a hard cap on the tentative window. If neither a confirm nor
         *  a clear echo verdict lands by here, default to FALSE_ALARM: ambiguity
         *  restores volume and never confirms, so it can't loop. */
        const val FALSE_ALARM_TICKS = 6

        /** Slow EMA rate for learning the echo gain UPWARD from ordinary
         *  below-threshold echo. Slow on purpose: a loud consonant — or the
         *  user's own voice leaking in below the trip threshold — must not swing
         *  the estimate up and raise the bar against a real interruption. */
        const val GAIN_ALPHA = 0.02

        /** Fast rate for learning the echo gain DOWNWARD (observed echo QUIETER
         *  than the current estimate). Asymmetric with [GAIN_ALPHA] by design and
         *  measured on-device: when the user is on a Bluetooth headset the reply
         *  plays in the headset, so almost nothing couples back into the mic
         *  (true ratio ~0.01) — yet a stale estimate learned earlier at speaker
         *  volume, or the 0.3 prior, left the gain too high and the onset bar
         *  above the user's own voice. Adapting down fast drops the bar to the
         *  real (near-zero) echo within ~half a second, while the slow upward
         *  rate still keeps the bar above genuine speaker echo. */
        const val GAIN_ALPHA_DOWN = 0.25

        /** Gain adaptation used when a tentative onset is judged to be echo (a
         *  FALSE_ALARM). Fed the clean [tripRatio] measured during the settle
         *  window (the actual speaker-into-mic coupling), so the onset bar climbs
         *  above it within a couple of such events rather than the many seconds
         *  [GAIN_ALPHA] would take. This convergence is what stops ordinary echo
         *  from repeatedly re-ducking the reply (the audible volume pumping). */
        const val FALSE_ALARM_GAIN_ALPHA = 0.3

        /** Floor low enough that a near-silent echo path (Bluetooth headset:
         *  ratio ~0.01) collapses the echo term (gain*playRef*margin) well under
         *  [absFloor] across the whole playRef range, so the onset bar rests at
         *  the absolute floor and an ordinary-volume interruption clears it. */
        const val GAIN_MIN = 0.02
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

    /** Clean mic/playRef coupling estimate for the current tentative onset —
     *  seeded at the trip and sharpened over the (still-full-volume) settle
     *  window. Both the residual line (tripRatio*duckVolume) and the FALSE_ALARM
     *  gain update key off it. */
    private var tripRatio = 0.0

    private var settleTicks = 0      // ticks since the duck; < SETTLE_TICKS = don't judge
    private var speechTicks = 0      // running post-settle ticks above the residual line
    private var echoTicks = 0        // running post-settle ticks on the residual line
    private var tentTotalTicks = 0   // total ticks in this tentative window (hard cap)

    // Last tick's mic RMS and the onset bar it faced, kept only so the Android
    // call sites can log "rms=… thr=… gain=…" on each barge event for on-device
    // sanity-checking. Pure reads.
    private var lastMicRms = 0.0
    private var lastThreshold = 0.0

    /** Snapshot of the diagnostics for a one-line logcat entry. */
    @Synchronized
    fun diagnostics(): String =
        "rms=%.4f thr=%.4f gain=%.3f playRef=%.4f trip=%.3f".format(
            lastMicRms, lastThreshold, gain, playRef, tripRatio,
        )

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
        // Mic level as a multiple of what's playing — the echo coupling if this
        // tick is just the reply bouncing back. Undefined when nothing is playing
        // (playRef ~ 0); guarded everywhere it feeds a decision.
        val playing = playRef > absFloor
        val ratio = if (playing) rms / playRef else 0.0

        val event = if (!tentative) {
            if (rms > threshold) {
                // Onset — start ducking and run the residual test from here.
                tentative = true
                tripRatio = ratio
                settleTicks = 0
                speechTicks = 0
                echoTicks = 0
                tentTotalTicks = 0
                Event.TENTATIVE
            } else {
                if (playing) learnGain(ratio)
                Event.NONE
            }
        } else {
            tentTotalTicks++
            when {
                // Settle: the duck is still draining the playback buffer, so the
                // echo is still full-volume — don't judge, just sharpen the clean
                // coupling estimate the residual line is built from.
                settleTicks < SETTLE_TICKS -> {
                    if (playing && ratio > tripRatio) tripRatio = ratio
                    settleTicks++
                    Event.NONE
                }
                // Hard timeout — resolve safely as echo (restore, never confirm).
                tentTotalTicks >= falseAlarmTicks -> falseAlarm()
                else -> {
                    // Ducked-residual test. Pure echo now sits at ~tripRatio*
                    // duckVolume; a real voice rides above it. When nothing is
                    // playing there's no echo to explain the energy, so any
                    // above-floor level is the user.
                    val isSpeech = if (playing) {
                        ratio > tripRatio * duckVolume * DUCK_RESIDUAL_MARGIN
                    } else {
                        rms > threshold
                    }
                    if (isSpeech) {
                        speechTicks++
                        echoTicks = 0
                        if (speechTicks >= minOnsetTicks) confirm() else Event.NONE
                    } else {
                        echoTicks++
                        speechTicks = 0
                        if (echoTicks >= DUCKED_ECHO_TICKS) falseAlarm() else Event.NONE
                    }
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
        clearTentative()
        return Event.CONFIRMED
    }

    /** Clears per-turn state (the playback reference, any in-flight tentative
     *  onset) for a fresh reply. Deliberately keeps the learned [gain] — it took
     *  several seconds of real playback to learn and stays valid across replies
     *  on the same device/volume/room. */
    @Synchronized
    fun reset() {
        playRef = 0.0
        clearTentative()
    }

    private fun clearTentative() {
        tentative = false
        tripRatio = 0.0
        settleTicks = 0
        speechTicks = 0
        echoTicks = 0
        tentTotalTicks = 0
    }

    /** A real voice held above the ducked-echo line long enough. */
    private fun confirm(): Event {
        clearTentative()
        return Event.CONFIRMED
    }

    /** The onset collapsed onto the ducked-echo line (or timed out) — it was our
     *  own echo. Fold the clean coupling into [gain] fast so the onset bar rises
     *  above it and stops re-ducking (the volume-pumping fix), then restore. */
    private fun falseAlarm(): Event {
        if (tripRatio > 0.0) learnGain(tripRatio, FALSE_ALARM_GAIN_ALPHA)
        clearTentative()
        return Event.FALSE_ALARM
    }

    /** Fold an observed mic/playback ratio into the learned echo [gain].
     *  Asymmetric when no explicit [alpha] is given: adapt DOWN fast
     *  ([GAIN_ALPHA_DOWN]) when the echo turns out quieter than the estimate, so
     *  a stale-high gain (e.g. after switching to a Bluetooth headset) can't keep
     *  the bar above the user's own voice; adapt UP slow ([GAIN_ALPHA]) so a loud
     *  consonant or below-threshold voice leak can't raise the bar. A confirmed
     *  false alarm passes [FALSE_ALARM_GAIN_ALPHA] explicitly to climb fast, from
     *  the clean coupling it just measured. */
    private fun learnGain(observedRatio: Double, alpha: Double = -1.0) {
        val a = when {
            alpha >= 0.0 -> alpha
            observedRatio < gain -> GAIN_ALPHA_DOWN
            else -> GAIN_ALPHA
        }
        gain = (gain + a * (observedRatio - gain)).coerceIn(GAIN_MIN, GAIN_MAX)
    }
}
