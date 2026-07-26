// Per-device hands-free tuning for the Chat page's voice mode. Deliberately
// localStorage-backed, NOT server settings: these are acoustic and
// room/device-dependent, so they belong to the browser they run in, not a
// value shared across every client. Mirrors the Android Prefs follow-up
// tunable.
import { FOLLOWUP_CAPTURE } from './wakeword.js'

const KEY = 'axon-voice-tuning'

export const VOICE_TUNING_DEFAULTS = Object.freeze({
  followupTicks: FOLLOWUP_CAPTURE.noSpeechTicks, // follow-up window length (×100ms)
  bargeEnabled: 0, // 0/1 — talk over a reply to interrupt it (relies on the browser's echo cancellation)
  bargeOnsetLevel: 50, // mic RMS×1000 the user must exceed to interrupt (raised bar keeps distant/quiet voices out)
})

// [min, max] clamps, matching the Android slider.
export const VOICE_TUNING_RANGES = Object.freeze({
  followupTicks: [30, 150],
  bargeEnabled: [0, 1],
  bargeOnsetLevel: [15, 200],
})

function clamp(v, [lo, hi]) {
  return Math.min(hi, Math.max(lo, v))
}

export function loadVoiceTuning() {
  let saved = {}
  try {
    saved = JSON.parse(localStorage.getItem(KEY) || '{}')
  } catch {
    saved = {}
  }
  const out = { ...VOICE_TUNING_DEFAULTS }
  for (const k of Object.keys(VOICE_TUNING_DEFAULTS)) {
    const v = Number(saved?.[k])
    if (Number.isFinite(v)) out[k] = clamp(v, VOICE_TUNING_RANGES[k])
  }
  return out
}

export function saveVoiceTuning(t) {
  try {
    localStorage.setItem(KEY, JSON.stringify(t))
  } catch {
    // storage unavailable — session-only tuning is fine
  }
}
