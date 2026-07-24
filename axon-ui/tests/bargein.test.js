import { describe, it, expect } from 'vitest'
import { createBargeDetector, BargeEvent } from '../src/lib/bargein.js'

// Drives `micTicks` through the detector, re-sampling a fixed playback level
// before every mic tick (the common case: one sentence mid-playback at a
// steady level). Returns the array of events, one per tick.
function drive(detector, playbackRms, micTicks) {
  return micTicks.map((mic) => {
    detector.feedPlayback(playbackRms)
    return detector.feedMic(mic)
  })
}

describe('BargeDetector', () => {
  it('never confirms on echo alone, well under the default-gain safety margin', () => {
    const detector = createBargeDetector()
    // A steady reply at RMS 0.1 echoing back at ratio 0.2 (0.02) — comfortably
    // under the default-gain threshold (0.3 * 0.1 * 2 = 0.06).
    const events = drive(detector, 0.1, Array(30).fill(0.02))
    expect(events).not.toContain(BargeEvent.TENTATIVE)
    expect(events).not.toContain(BargeEvent.CONFIRMED)
  })

  it('never confirms on echo alone as the gain converges to a louder device', () => {
    const detector = createBargeDetector()
    // This device's echo is louder than the default prior (ratio 0.5 vs the
    // default 0.3) — the threshold only ever rises toward it (monotonically,
    // from the safe side), so it can never dip below the actual echo level.
    const events = drive(detector, 0.1, Array(200).fill(0.05))
    expect(events).not.toContain(BargeEvent.TENTATIVE)
    expect(events).not.toContain(BargeEvent.CONFIRMED)
  })

  it('confirms real speech held over the echo within a few ticks', () => {
    const detector = createBargeDetector()
    // Warm up on plain echo (ratio 0.3, matching the default gain) so the
    // learned threshold reflects a real device before the user interrupts.
    drive(detector, 0.1, Array(20).fill(0.03))
    const events = drive(detector, 0.1, [0.2, 0.2, 0.2, 0.2])
    expect(events[0]).toBe(BargeEvent.TENTATIVE)
    // Confirms by the 3rd loud tick — ~300ms after onset at the 100ms
    // cadence, comfortably inside the ~400ms budget.
    expect(events.indexOf(BargeEvent.CONFIRMED)).toBe(2)
  })

  it('restores (false alarm) when a brief onset fades and never holds up', () => {
    const detector = createBargeDetector()
    drive(detector, 0.1, Array(20).fill(0.03))
    const onset = drive(detector, 0.1, [0.2])
    expect(onset[0]).toBe(BargeEvent.TENTATIVE)
    // A cough: one loud tick, then back under threshold and staying there.
    const fade = drive(detector, 0.1, Array(6).fill(0.03))
    expect(fade).toContain(BargeEvent.FALSE_ALARM)
    expect(fade).not.toContain(BargeEvent.CONFIRMED)
  })

  it('a wake-word hit confirms immediately during a duck, bypassing the onset window', () => {
    const detector = createBargeDetector()
    drive(detector, 0.1, Array(20).fill(0.03))
    const onset = drive(detector, 0.1, [0.2])
    expect(onset[0]).toBe(BargeEvent.TENTATIVE)
    expect(detector.wakeWordHit()).toBe(BargeEvent.CONFIRMED)
  })

  it('reset clears per-turn state but keeps the learned gain', () => {
    const trained = createBargeDetector()
    // This device's echo is much quieter than the default prior (ratio 0.1
    // vs the default 0.3) — warm it up on plain echo so gain adapts down.
    drive(trained, 0.1, Array(200).fill(0.01))
    trained.reset()

    // A moderate interruption that clears the *trained* threshold (~0.02)
    // but sits under the untrained default threshold (~0.06) — proof the
    // learned gain, not just the default, is what's active after reset.
    const fresh = createBargeDetector()
    const trainedEvents = drive(trained, 0.1, [0.03])
    const freshEvents = drive(fresh, 0.1, [0.03])
    expect(trainedEvents[0]).toBe(BargeEvent.TENTATIVE)
    expect(freshEvents[0]).toBe(BargeEvent.NONE)
  })

  it('lets the threshold fall back toward the floor once playback stops', () => {
    const detector = createBargeDetector()
    drive(detector, 0.1, Array(20).fill(0.03)) // warm up on echo, gain ~0.3
    // The reply stream ends (PcmPlayback's onLevel(-1) convention): playback
    // stops raising the reference, and the peak-hold decays back toward the
    // absolute floor over the next several ticks rather than resetting
    // instantly — a genuine short gap mid-reply shouldn't misread as one.
    const events = drive(detector, -1, Array(10).fill(0.05))
    expect(events).toContain(BargeEvent.TENTATIVE)
  })
})
