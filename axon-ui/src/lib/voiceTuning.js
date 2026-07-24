// Per-device barge-in / hands-free tuning for the Chat page's voice mode.
// Deliberately localStorage-backed, NOT server settings: these are acoustic and
// room/device-dependent (echo coupling, mic distance), so they belong to the
// browser they run in, not a value shared across every client. Mirrors the
// Android Prefs barge tunables; defaults equal bargein.js's own constants, so an
// untouched install behaves exactly as before.
import { MARGIN, MIN_ONSET_TICKS, FLATNESS_MAX } from './bargein.js'
import { FOLLOWUP_CAPTURE } from './wakeword.js'

const KEY = 'axon-voice-tuning'

export const VOICE_TUNING_DEFAULTS = Object.freeze({
  margin: MARGIN, // echo rejection: lower = easier to interrupt
  speechThreshold: FLATNESS_MAX, // cough/clap filter (both flatness + ZCR ceilings)
  onsetTicks: MIN_ONSET_TICKS, // interrupt hold (×100ms)
  followupTicks: FOLLOWUP_CAPTURE.noSpeechTicks, // follow-up window length (×100ms)
})

// [min, max] clamps, matching the Android sliders.
export const VOICE_TUNING_RANGES = Object.freeze({
  margin: [1.2, 3.0],
  speechThreshold: [0.2, 0.5],
  onsetTicks: [1, 8],
  followupTicks: [30, 150],
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
