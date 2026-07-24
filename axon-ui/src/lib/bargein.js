// Duck-then-confirm barge-in detector: decides when the user is trying to
// interrupt an in-progress spoken reply. The hard part is that the mic hears
// the reply's own echo bouncing off the room/speaker along with any real
// speech, so a plain "mic got loud" trigger fires on the assistant's own
// voice. This separates the two by learning how loud that echo is relative to
// what's actually playing (`gain`) — per device, per volume, per room, no
// calibration step — and requires speech to hold up *after* the reply has
// been ducked down, where a real interruption keeps going and a stray echo or
// cough does not.
//
// On top of that, feedMic takes an optional speechShaped flag (see
// looksLikeSpeech below) so a loud tick only counts toward the onset/confirm
// hold if it's also shaped like voiced speech rather than a broadband burst
// (cough, clap, mic pop) — energy alone can't tell those apart, since a loud
// cough clears the same threshold a real interruption does.
//
// State machine was previously mirrored line-for-line in
// axon-voice/app/src/main/java/com/axon/voice/audio/BargeDetector.kt (same
// convention as wakeword.js / SilenceWatcher); the two are diverging now that
// Android is moving to on-device speaker verification instead of a spectral
// shape gate — a heavier but stronger check that a browser can't run as
// cheaply. The duck/confirm/false-alarm shape below still applies to both:
//
//   idle      --[mic over threshold AND speech-shaped]--> tentative (caller ducks the reply)
//   tentative --[holds MIN_ONSET_TICKS]------------------> event=CONFIRMED  (caller stops + listens)
//   tentative --[falls back within FALSE_ALARM_TICKS]----> event=FALSE_ALARM (caller restores volume)
//   any state --[wakeWordHit()]---------------------------> event=CONFIRMED
//
// The wake word always confirms outright rather than going through the
// tentative window: once the reply is ducked it is no longer fooled by echo,
// and during the silent "thinking" phase there is no playback reference at
// all for the mic-RMS path to work with, so the wake word is the only signal.
//
// feedMic must be called on a steady ~100ms cadence — the same cadence
// wakeword.js's watchSilence already uses — because MIN_ONSET_TICKS /
// FALSE_ALARM_TICKS are tick counts, not durations. feedPlayback can be
// called at any rate; only the latest value before each feedMic tick matters.
// Deliberately clock-free (no Date.now()/setTimeout inside): every call is
// driven by the caller, so a test can feed a synthetic tick trace without
// wall-clock timing.
//
// speechShaped (feedMic's 2nd arg, default true for callers that don't have a
// shape signal) only ever gates whether a loud tick counts toward starting or
// extending an onset — it never touches learnGain, which still fires on every
// tick that's genuinely below threshold regardless of shape, so a
// misclassified cough can't skew the learned echo gain. A tick that's loud
// but not speech-shaped, mid-onset, is treated exactly like a quiet tick:
// it pushes toward FALSE_ALARM rather than resetting the onset outright,
// so one misjudged tick inside a real interruption (e.g. a leading fricative)
// costs at most a little delay, not a dropped confirm. But that tolerance is
// deliberately narrow — ONSET_MISS_GRACE below caps it at one isolated tick,
// not an unbounded number scattered across the whole tentative window. A
// real loud cough is rarely one clean broadband burst: forceful coughs often
// have a brief voiced release that can slip past the shape gate for a tick,
// and a coughing fit is several such bursts in a row. Without a cap, three of
// those stray qualifying ticks — even minutes apart, as long as no single
// gap reaches FALSE_ALARM_TICKS — would still accumulate to a false CONFIRMED.
// Requiring the misses between qualifying ticks to be isolated (never two in
// a row) keeps the one-fricative tolerance for real speech while denying a
// cough's sparse, mostly-unshaped pattern the same leniency.

export const ABS_FLOOR = 0.025 // mirrors FOLLOWUP_RMS in wakeword.js
export const MARGIN = 2.0
export const MIN_ONSET_TICKS = 3 // ~300ms at the standard 100ms tick cadence
export const FALSE_ALARM_TICKS = 6 // ~600ms
export const ONSET_MISS_GRACE = 1 // at most one isolated non-qualifying tick tolerated mid-onset
export const GAIN_ALPHA = 0.02 // slow EMA — learns over seconds, not one tick
export const GAIN_MIN = 0.05
export const GAIN_MAX = 5.0
export const GAIN_DEFAULT = 0.3
export const PLAYREF_DECAY = 0.85

// Unvalidated starting points, not measured against real recordings — expect
// to retune both against actual cough/speech/noise samples once this ships.
// Spectral flatness (Wiener entropy) of clean voiced speech is typically well
// under 0.3; broadband bursts (coughs, claps, pops) run flatter, toward 1.
// Zero-crossing rate follows the same split for the same reason: a burst has
// no dominant pitch period to keep crossings low and regular.
export const FLATNESS_MAX = 0.35
export const ZCR_MAX = 0.35

export const BargeEvent = Object.freeze({
  NONE: 'none',
  TENTATIVE: 'tentative',
  CONFIRMED: 'confirmed',
  FALSE_ALARM: 'false_alarm',
})

// True when a tick's spectral shape reads as voiced speech rather than a
// broadband burst. Requires BOTH features to look speech-like (stricter than
// either alone) — deliberately conservative about calling something "not
// speech", because the state machine already tolerates an occasional
// misclassified tick inside a real interruption (see the module doc); it does
// NOT tolerate a cough being let all the way through.
export function looksLikeSpeech(
  { flatness, zcr },
  { flatnessMax = FLATNESS_MAX, zcrMax = ZCR_MAX } = {}
) {
  return flatness < flatnessMax && zcr < zcrMax
}

export function createBargeDetector({
  absFloor = ABS_FLOOR,
  margin = MARGIN,
  minOnsetTicks = MIN_ONSET_TICKS,
  falseAlarmTicks = FALSE_ALARM_TICKS,
  onsetMissGrace = ONSET_MISS_GRACE,
} = {}) {
  let playRef = 0
  let gain = GAIN_DEFAULT
  let tentative = false
  let onsetTicks = 0
  let quietTicks = 0
  let missStreak = 0 // consecutive non-qualifying ticks since the last qualifying one

  // A negative value (the same "nothing playing" convention PcmPlayback's
  // onLevel and the web envelope both use) never raises the peak-hold, but
  // doesn't reset it either — it fires between every back-to-back sentence,
  // not just at the reply's true end, so a hard reset here would zero the
  // echo reference at every sentence boundary and let a room's reverb tail
  // read as a fresh interruption; PLAYREF_DECAY already governs how fast the
  // reference actually falls.
  function feedPlayback(rms) {
    const level = rms < 0 ? 0 : rms
    if (level > playRef) playRef = level
  }

  function learnGain(micRms) {
    const observed = micRms / playRef
    gain = Math.min(GAIN_MAX, Math.max(GAIN_MIN, gain + GAIN_ALPHA * (observed - gain)))
  }

  function feedMic(rms, speechShaped = true) {
    const threshold = Math.max(absFloor, gain * playRef * margin)
    const loud = rms > threshold
    const qualifies = loud && speechShaped
    let event = BargeEvent.NONE
    if (!tentative) {
      if (qualifies) {
        tentative = true
        onsetTicks = 1
        quietTicks = 0
        missStreak = 0
        event = BargeEvent.TENTATIVE
      } else {
        // Only ever learn from ticks that are genuinely quiet (below
        // threshold) — a loud-but-unshaped tick (a cough) must not reach
        // learnGain just because it failed the shape check, or one bad
        // sample could skew the learned echo gain.
        if (!loud && playRef > absFloor) learnGain(rms)
      }
    } else if (qualifies) {
      onsetTicks++
      quietTicks = 0
      missStreak = 0
      if (onsetTicks >= minOnsetTicks) {
        tentative = false
        onsetTicks = 0
        event = BargeEvent.CONFIRMED
      }
    } else {
      quietTicks++
      missStreak++
      // More than the tolerated isolated miss in a row: this onset's
      // qualifying ticks aren't holding together as one continuous
      // interruption, so give up its progress rather than let it keep
      // accumulating indefinitely (see the module doc — this is what stops
      // a sparse, mostly-unshaped cough or coughing fit from eventually
      // reaching minOnsetTicks one stray tick at a time).
      if (missStreak > onsetMissGrace) onsetTicks = 0
      if (quietTicks >= falseAlarmTicks) {
        tentative = false
        onsetTicks = 0
        quietTicks = 0
        event = BargeEvent.FALSE_ALARM
      }
    }
    // Decay the peak-hold AFTER this tick's threshold/learning used it — the
    // reference for the *next* tick, not this one.
    playRef *= PLAYREF_DECAY
    return event
  }

  // The wake word fired — always an immediate, unconditional confirm. See
  // the module doc for why this bypasses the tentative window entirely.
  function wakeWordHit() {
    tentative = false
    onsetTicks = 0
    quietTicks = 0
    missStreak = 0
    return BargeEvent.CONFIRMED
  }

  // Clears per-turn state (the playback reference, any in-flight tentative
  // onset) for a fresh reply. Deliberately keeps the learned gain — it took
  // several seconds of real playback to learn and stays valid across replies
  // on the same device/volume/room.
  function reset() {
    playRef = 0
    tentative = false
    onsetTicks = 0
    quietTicks = 0
    missStreak = 0
  }

  return { feedPlayback, feedMic, wakeWordHit, reset }
}
