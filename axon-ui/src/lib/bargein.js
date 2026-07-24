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
// State machine (mirrored line-for-line in
// axon-voice/app/src/main/java/com/axon/voice/audio/BargeDetector.kt — keep
// the two in step, same convention as wakeword.js / SilenceWatcher):
//
//   idle      --[mic over threshold]--------------> tentative (caller ducks the reply)
//   tentative --[holds MIN_ONSET_TICKS]------------> event=CONFIRMED  (caller stops + listens)
//   tentative --[falls back within FALSE_ALARM_TICKS]-> event=FALSE_ALARM (caller restores volume)
//   any state --[wakeWordHit()]---------------------> event=CONFIRMED
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

export const ABS_FLOOR = 0.025 // mirrors FOLLOWUP_RMS in wakeword.js
export const MARGIN = 2.0
export const MIN_ONSET_TICKS = 3 // ~300ms at the standard 100ms tick cadence
export const FALSE_ALARM_TICKS = 6 // ~600ms
export const GAIN_ALPHA = 0.02 // slow EMA — learns over seconds, not one tick
export const GAIN_MIN = 0.05
export const GAIN_MAX = 5.0
export const GAIN_DEFAULT = 0.3
export const PLAYREF_DECAY = 0.85

export const BargeEvent = Object.freeze({
  NONE: 'none',
  TENTATIVE: 'tentative',
  CONFIRMED: 'confirmed',
  FALSE_ALARM: 'false_alarm',
})

export function createBargeDetector({
  absFloor = ABS_FLOOR,
  margin = MARGIN,
  minOnsetTicks = MIN_ONSET_TICKS,
  falseAlarmTicks = FALSE_ALARM_TICKS,
} = {}) {
  let playRef = 0
  let gain = GAIN_DEFAULT
  let tentative = false
  let onsetTicks = 0
  let quietTicks = 0

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

  function feedMic(rms) {
    const threshold = Math.max(absFloor, gain * playRef * margin)
    let event = BargeEvent.NONE
    if (!tentative) {
      if (rms > threshold) {
        tentative = true
        onsetTicks = 1
        quietTicks = 0
        event = BargeEvent.TENTATIVE
      } else {
        if (playRef > absFloor) learnGain(rms)
      }
    } else if (rms > threshold) {
      onsetTicks++
      quietTicks = 0
      if (onsetTicks >= minOnsetTicks) {
        tentative = false
        onsetTicks = 0
        event = BargeEvent.CONFIRMED
      }
    } else {
      quietTicks++
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
  }

  return { feedPlayback, feedMic, wakeWordHit, reset }
}
