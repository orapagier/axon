// Shared Web Audio plumbing for the hands-free voice orb (see VoiceOrb.vue):
// turns a live AnalyserNode — the wake mic's or a TTS <audio> element's — into
// a 0..1-ish RMS level the orb can react to. One AudioContext is reused for
// every tapped <audio> element so we don't accumulate one per TTS sentence.
let audioCtx = null

export function ensureAudioCtx() {
  if (!audioCtx) {
    const Ctx = window.AudioContext || window.webkitAudioContext
    audioCtx = new Ctx()
  }
  if (audioCtx.state === 'suspended') audioCtx.resume()
  return audioCtx
}

// Taps a fresh <audio> element's output with an AnalyserNode so TTS playback
// can drive the same amplitude-reactive animation as the mic.
//
// Deliberately uses captureStream() -> createMediaStreamSource(), NOT
// createMediaElementSource(el). The latter reroutes the element's actual
// output through the Web Audio graph — reconnecting to ctx.destination is
// required to keep it audible, and if this AudioContext isn't confirmed
// 'running' yet (e.g. wake word auto-restarted from a saved toggle on page
// load, with no fresh click in this session to satisfy the autoplay-unlock
// requirement) that reconnect silently produces no sound while the element
// still looks like it's playing. captureStream() instead hands us a copy of
// the element's audio for analysis only; the element keeps playing through
// its own normal output path completely untouched, so a suspended analysis
// context can only make the orb less reactive, never mute the reply.
export function tapElement(el) {
  if (typeof el.captureStream !== 'function') return null // Safari etc. — orb falls back to a synthetic pulse
  const ctx = ensureAudioCtx()
  const stream = el.captureStream()
  const source = ctx.createMediaStreamSource(stream)
  const analyser = ctx.createAnalyser()
  analyser.fftSize = 512
  analyser.smoothingTimeConstant = 0.6
  source.connect(analyser)
  return analyser
}

const levelBuf = new Float32Array(2048)

// RMS of the analyser's current time-domain buffer, roughly 0..1 (speech
// rarely pushes this much past 0.3 — callers scale up for visual punch).
export function readLevel(analyser) {
  if (!analyser) return 0
  const n = Math.min(levelBuf.length, analyser.fftSize)
  const data = levelBuf.subarray(0, n)
  analyser.getFloatTimeDomainData(data)
  let acc = 0
  for (let i = 0; i < n; i++) acc += data[i] * data[i]
  return Math.sqrt(acc / n)
}
