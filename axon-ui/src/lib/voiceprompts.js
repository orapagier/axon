// Spoken acknowledgments for the wake word flow: a short "Yes?" when "Hey
// Axon" is heard and "Anything else?" before the follow-up window, so the
// assistant answers with a voice instead of only a chime + recording timer.
//
// Playback must be instant — it gates when the user can start talking — so the
// phrases are synthesized once through the configured tts.* endpoint (same
// voice as spoken replies) and cached as object URLs. When a phrase isn't
// cached (TTS unconfigured, prefetch still in flight, network down) playPrompt
// falls back to the browser's speechSynthesis; when that can't speak either it
// resolves false and the caller plays the old chime.
import { postRaw } from './api.js'

export const WAKE_ACKS = ['Yes?', 'Mm-hmm?', "I'm listening."]
export const FOLLOWUP_PROMPT = 'Anything else?'

const cache = new Map() // phrase -> object URL of the server-TTS audio blob
const inflight = new Set() // phrases with a fetch already running

export function randomWakeAck() {
  return WAKE_ACKS[Math.floor(Math.random() * WAKE_ACKS.length)]
}

// Fire-and-forget: warm the cache for every phrase. Safe to call repeatedly —
// cached and in-flight phrases are skipped, so failed fetches (TTS not
// configured yet, server briefly down) retry on the next call rather than
// looping.
export function prefetchPrompts() {
  for (const text of [...WAKE_ACKS, FOLLOWUP_PROMPT]) {
    if (cache.has(text) || inflight.has(text)) continue
    inflight.add(text)
    ;(async () => {
      try {
        const res = await postRaw('/audio/speech', { text })
        const type = res.headers.get('content-type') || ''
        if (res.ok && type.startsWith('audio/')) {
          cache.set(text, URL.createObjectURL(await res.blob()))
        }
      } catch {
        // Unreachable/unconfigured TTS — playPrompt falls back to synthesis.
      } finally {
        inflight.delete(text)
      }
    })()
  }
}

let pinnedUtterance = null // Chrome goes silent if the utterance is GC'd

function playCached(text) {
  const url = cache.get(text)
  if (!url) return Promise.resolve(false)
  return new Promise((resolve) => {
    // The watchdog resolves even if the element never fires ended/error
    // (phrases are 1-2 words; anything past 4s means events were swallowed).
    const watchdog = setTimeout(() => resolve(true), 4000)
    const done = (ok) => {
      clearTimeout(watchdog)
      resolve(ok)
    }
    const el = new Audio(url)
    el.onended = () => done(true)
    el.onerror = () => done(false)
    el.play().catch(() => done(false)) // autoplay blocked before any gesture
  })
}

function playSynthesis(text) {
  if (typeof window === 'undefined' || !('speechSynthesis' in window)) {
    return Promise.resolve(false)
  }
  return new Promise((resolve) => {
    const synth = window.speechSynthesis
    const u = new SpeechSynthesisUtterance(text)
    pinnedUtterance = u
    let watchdog = null
    const finish = (ok) => {
      clearTimeout(watchdog)
      if (pinnedUtterance === u) pinnedUtterance = null
      resolve(ok)
    }
    watchdog = setTimeout(() => finish(true), 4000)
    u.onend = () => finish(true)
    u.onerror = () => finish(false) // e.g. 'not-allowed' before any gesture
    synth.speak(u)
    synth.resume() // Chrome: queue can come back from cancel() stuck paused
  })
}

// Resolves once the phrase has finished playing (so callers can defer the
// silence watcher until the speakers are quiet); resolves false when nothing
// audible could be produced, letting the caller chime instead.
export async function playPrompt(text) {
  if (await playCached(text)) return true
  return playSynthesis(text)
}
