// The assistant's own short phrases: a "Yes?" ack when "Hey Axon" is heard — so
// the assistant answers with a voice instead of a chime and silence.
//
// Deliberately just the acks. Earlier versions also rotated four thinking
// fillers ("Let me check.", "On it."…) while a run worked and spoke "Anything
// else?" to open the follow-up window; hearing a stock phrase on every turn
// read as chatter rather than conversation. Only the acks survive — they
// answer *you*, which is why they stay varied — and the follow-up window is
// announced by its soft chime alone. Don't reintroduce filler phrases here:
// dead air during a run is the intended behavior, not a gap to paper over.
//
// Playback must be instant — it gates when the user can start talking — so the
// phrases are synthesized once through the configured tts.* endpoint (same
// voice as spoken replies) and cached as object URLs. A miss synthesizes on
// demand rather than dropping straight to the browser voice, which sounds
// nothing like the replies; only when that fails or times out does playPrompt
// fall back to speechSynthesis, and when that can't speak either it resolves
// false and the caller plays the old chime.
import { postRaw } from './api.js'

export const WAKE_ACKS = ['Yes?', 'Mm-hmm?', "I'm listening."]

const ALL_PROMPTS = [...WAKE_ACKS]

const cache = new Map() // phrase -> object URL of the server-TTS audio blob
const inflight = new Map() // phrase -> Promise<boolean> of a fetch in progress

// How long playPrompt waits on an on-demand synthesis before giving up and
// letting the browser voice cover the gap. The phrase gates the conversation,
// so a slow or unreachable endpoint must not stall it — but the fetch keeps
// running and populates the cache, so the next use is in the right voice.
const ON_DEMAND_MS = 2000

export function randomWakeAck() {
  return WAKE_ACKS[Math.floor(Math.random() * WAKE_ACKS.length)]
}

// Resolves true once the phrase is cached. Concurrent callers (the prefetch
// and an on-demand miss) share one request instead of each issuing their own.
// A failed fetch leaves nothing behind, so the next call retries rather than
// caching the failure.
function fetchPhrase(text) {
  if (cache.has(text)) return Promise.resolve(true)
  const running = inflight.get(text)
  if (running) return running
  const p = (async () => {
    try {
      const res = await postRaw('/audio/speech', { text })
      const type = res.headers.get('content-type') || ''
      if (res.ok && type.startsWith('audio/')) {
        cache.set(text, URL.createObjectURL(await res.blob()))
        return true
      }
    } catch {
      // Unreachable/unconfigured TTS — playPrompt falls back to synthesis.
    } finally {
      inflight.delete(text)
    }
    return false
  })()
  inflight.set(text, p)
  return p
}

// Fire-and-forget: warm the cache for every phrase. Safe to call repeatedly —
// cached and in-flight phrases are skipped, so failed fetches (TTS not
// configured yet, server briefly down) retry on the next call rather than
// looping.
export function prefetchPrompts() {
  for (const text of ALL_PROMPTS) fetchPhrase(text)
}

let pinnedUtterance = null // Chrome goes silent if the utterance is GC'd
let activePrompt = null // { stop() } for the phrase currently coming out
let promptSeq = 0 // bumping this invalidates a playPrompt still getting ready

// Cut off whatever prompt is playing — used when the spoken reply takes over
// the speaker, so a late ack can't overlap it. The pending playPrompt
// resolves true: audio *was* produced, just cut short, so the caller must not
// treat it as "nothing could speak" and chime.
//
// The sequence bump matters as much as the stop: a prompt cancelled while it
// is still synthesizing has no audio to silence yet, and without it the fetch
// would land afterwards and start talking over the reply.
export function stopPrompt() {
  promptSeq += 1
  const p = activePrompt
  activePrompt = null
  if (p) p.stop()
}

function playCached(text) {
  const url = cache.get(text)
  if (!url) return Promise.resolve(false)
  return new Promise((resolve) => {
    const el = new Audio(url)
    let watchdog = null
    const done = (ok) => {
      clearTimeout(watchdog)
      if (activePrompt === handle) activePrompt = null
      resolve(ok) // later calls are no-ops: the promise is already settled
    }
    const handle = {
      stop() {
        try {
          el.pause()
        } catch {
          // element already torn down — nothing to silence
        }
        done(true)
      },
    }
    // The watchdog resolves even if the element never fires ended/error
    // (phrases are 1-2 words; anything past 4s means events were swallowed).
    watchdog = setTimeout(() => done(true), 4000)
    el.onended = () => done(true)
    el.onerror = () => done(false)
    activePrompt = handle
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
      if (activePrompt === handle) activePrompt = null
      resolve(ok)
    }
    const handle = {
      stop() {
        synth.cancel()
        finish(true)
      },
    }
    watchdog = setTimeout(() => finish(true), 4000)
    u.onend = () => finish(true)
    u.onerror = () => finish(false) // e.g. 'not-allowed' before any gesture
    activePrompt = handle
    synth.speak(u)
    synth.resume() // Chrome: queue can come back from cancel() stuck paused
  })
}

// Resolves once the phrase has finished playing (so callers can defer the
// silence watcher until the speakers are quiet); resolves false when nothing
// audible could be produced, letting the caller chime instead. A stopPrompt()
// at any point resolves true — cut short still counts as spoken, and a chime
// on top of the reply is exactly what the stop was avoiding.
export async function playPrompt(text) {
  const seq = promptSeq
  if (await playCached(text)) return true
  if (seq !== promptSeq) return true
  // Cache miss — the prefetch is still in flight, never ran, or failed earlier.
  // Synthesize now so the phrase comes out in the configured tts.* voice
  // instead of the browser's, which sounds nothing like the spoken replies.
  const fetched = await Promise.race([
    fetchPhrase(text),
    new Promise((r) => setTimeout(() => r(false), ON_DEMAND_MS)),
  ])
  if (seq !== promptSeq) return true // reply took the speaker while we waited
  if (fetched && (await playCached(text))) return true
  if (seq !== promptSeq) return true
  return playSynthesis(text)
}
