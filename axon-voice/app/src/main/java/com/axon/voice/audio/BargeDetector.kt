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
 * The energy/gain state machine below is the same shape as
 * axon-ui/src/lib/bargein.js, but the two platforms now diverge: web layers a
 * cheap spectral speech-shape gate directly into `feedMic` (a browser can't
 * afford much more per 100ms tick), while Android layers a heavier, stronger
 * check — [BargeMonitor]'s `verifySpeaker` runs a CAM++ speaker-embedding
 * match ([SpeakerEmbedder]) once, right as an energy-based CONFIRMED fires
 * here, rejecting anyone who isn't the enrolled user ([VoicePrint]) instead of
 * just anyone who isn't speech-shaped. Android also nudges the learned echo
 * [gain] up on a FALSE_ALARM (see [FALSE_ALARM_GAIN_BOOST]) so a low estimate
 * can't leave the reply's volume pumping; web still relies on [learnGain]
 * alone. Same convention as [SilenceWatcher] / wakeword.js:
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
    margin: Double = MARGIN,
    private val minOnsetTicks: Int = MIN_ONSET_TICKS,
    private val falseAlarmTicks: Int = FALSE_ALARM_TICKS,
    private val onsetMissGrace: Int = ONSET_MISS_GRACE,
) {
    enum class Event { NONE, TENTATIVE, CONFIRMED, FALSE_ALARM }

    // Live-tunable knobs (see [tune]); start at the constants but every one of
    // these is now exposed in Settings > barge-in tuning and re-read per reply,
    // because the right value is device/room/volume-specific and can't be
    // guessed once. Held as vars, not constructor vals, so the long-lived
    // detector both WakeWordService and ChatActivity keep can pick up a
    // slider change on the next reply without being rebuilt.
    private var margin: Double = margin
    private var falseAlarmGainBoost: Double = FALSE_ALARM_GAIN_BOOST
    private var playrefDecay: Double = PLAYREF_DECAY

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

        /** Ceiling on the learned echo [gain]. Lowered from an original 5.0:
         *  at 5.0 a few [FALSE_ALARM_GAIN_BOOST] bumps during a pumping episode
         *  drove the threshold (`gain*playRef*margin`) up to ~10x the digital
         *  playback level — high enough that a real interruption at a normal
         *  voice level physically couldn't cross it, so barge-in went dead
         *  until [learnGain] slowly decayed the gain back down (at which point
         *  it re-pumped). 2.5 caps the worst-case threshold at ~5x playback:
         *  still well clear of ordinary echo, but a close, normal-volume voice
         *  can still get over it. The energy gate is only the first of two —
         *  the speaker-embedding check still has the final say — so a slightly
         *  looser cap here doesn't loosen who can actually interrupt. */
        const val GAIN_MAX = 2.5

        /** Multiplicative bump applied to the learned echo [gain] on every
         *  FALSE_ALARM (the default; user-overridable via [tune] from the
         *  Settings "Echo suppression" slider — 1.0 disables it). A false alarm
         *  means the loud tick that made us duck faded out once ducked — it was
         *  the reply's own echo, not a real interruption, so the echo estimate
         *  that set the threshold was too low. The slow per-tick [learnGain]
         *  can't fix that here: it only runs on non-tentative quiet ticks, so
         *  once the echo is loud enough to keep tripping TENTATIVE the detector
         *  oscillates duck<->restore (the reply's volume audibly "pumping") and
         *  never learns its way out. Bumping [gain] geometrically per false
         *  alarm lifts the threshold back above the echo within a cycle or two;
         *  the slow [learnGain] then settles it at the true ratio and it holds
         *  (kept across replies by [reset]). Bounded by [GAIN_MAX] and
         *  self-limiting, so one loud transient can't slam the gain the way
         *  learning straight toward its raw ratio would. Lowered from an
         *  original 2.0 to 1.5: 2.0 overshot hard enough (in tandem with the
         *  old 5.0 [GAIN_MAX]) to starve real barge-ins between pumping bouts. */
        const val FALSE_ALARM_GAIN_BOOST = 1.5

        /** Default prior before any learning has happened: a phone's own
         *  speaker-into-own-mic echo is typically well below unity gain. */
        const val GAIN_DEFAULT = 0.3

        /** Peak-hold decay applied once per [feedMic] tick regardless of how
         *  often [feedPlayback] fires — including a genuine gap where it
         *  doesn't fire at all, e.g. [PcmPlayback] tearing down and building a
         *  fresh [android.media.MediaCodec] between two streamed sentences.
         *  At the original 0.85 (~430ms half-life), that gap decayed the
         *  reference substantially before the next sentence's own echo
         *  arrived, so the threshold — computed from the now-artificially-low
         *  reference — read ordinary continuing playback as loud enough to
         *  tentatively confirm, duck, then false-alarm back a few hundred ms
         *  later: the reply's volume visibly pumping at every sentence
         *  boundary. 0.94 (~1.1s half-life) gives the reference enough runway
         *  to survive a real pause without depending on decode/codec timing. */
        const val PLAYREF_DECAY = 0.94
    }

    private var playRef = 0.0
    private var gain = GAIN_DEFAULT

    private var tentative = false
    private var onsetTicks = 0
    private var quietTicks = 0
    private var missStreak = 0 // consecutive non-loud ticks since the last loud one, while tentative

    // Last tick's mic RMS and the threshold it was compared against, kept only
    // so the Android call sites can log "rms=… thr=… gain=…" on each barge
    // event — the energy gate used to be a black box (only the speaker check
    // logged), leaving nothing to tune the Settings sliders against on real
    // hardware. Pure reads; kept off the companion so a JVM test can still
    // exercise this class with no android.util.Log dependency.
    private var lastMicRms = 0.0
    private var lastThreshold = 0.0

    /** Snapshot of the diagnostics for a one-line logcat entry (see the
     *  Android barge callbacks): last mic RMS, the threshold it faced, and the
     *  current learned echo gain. */
    @Synchronized
    fun diagnostics(): String =
        "rms=%.4f thr=%.4f gain=%.3f playRef=%.4f margin=%.2f".format(
            lastMicRms, lastThreshold, gain, playRef, margin
        )

    /** Apply the live Settings tunables. Called alongside [reset] at the start
     *  of every reply so a slider change takes effect on the next reply with no
     *  restart — the detector itself is long-lived (its learned [gain] is worth
     *  keeping), so the knobs ride in this way rather than through the
     *  constructor. Leaves the learned [gain] untouched; [reset] handles that.
     *  Web (`bargein.js`) deliberately does not mirror these — it uses a
     *  spectral gate, not this speaker-embedding path, and the user only tunes
     *  Android. */
    @Synchronized
    fun tune(margin: Double, falseAlarmGainBoost: Double, playrefDecay: Double) {
        this.margin = margin
        this.falseAlarmGainBoost = falseAlarmGainBoost
        this.playrefDecay = playrefDecay
    }

    /** Whether the reply was actually playing (playRef above the absolute
     *  floor) at the tick that opened the current tentative onset. Gates the
     *  FALSE_ALARM gain bump so a barge attempt during the silent "thinking"
     *  phase — real speech, no echo reference at all — can never be mistaken
     *  for an echo overshoot and mistrain the echo gain. */
    private var onsetHadPlayback = false

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
        lastMicRms = rms
        lastThreshold = threshold
        val event = if (!tentative) {
            if (rms > threshold) {
                tentative = true
                onsetTicks = 1
                quietTicks = 0
                missStreak = 0
                onsetHadPlayback = playRef > absFloor
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
                    // The loud onset that ducked us faded once ducked — echo,
                    // not a real interruption. Lift the echo gain so ordinary
                    // playback stops crossing the threshold; otherwise the
                    // detector keeps ducking and restoring and the reply's
                    // volume pumps. Only when the reply was actually playing at
                    // onset (never off a thinking-phase barge attempt).
                    if (onsetHadPlayback) {
                        gain = (gain * falseAlarmGainBoost).coerceAtMost(GAIN_MAX)
                    }
                    Event.FALSE_ALARM
                } else {
                    Event.NONE
                }
            }
        }
        // Decay the peak-hold AFTER this tick's threshold/learning used it —
        // the reference for the *next* tick, not this one.
        playRef *= playrefDecay
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
        onsetHadPlayback = false
    }

    private fun learnGain(micRms: Double) {
        val observed = micRms / playRef
        gain = (gain + GAIN_ALPHA * (observed - gain)).coerceIn(GAIN_MIN, GAIN_MAX)
    }
}
