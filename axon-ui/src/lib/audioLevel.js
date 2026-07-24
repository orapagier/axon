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
// can drive the same amplitude-reactive animation as the mic. Once tapped, the
// element's audio is routed through the Web Audio graph instead of playing
// directly — reconnecting to destination keeps it audible. Only ever call this
// once per element (StreamingSpeech/toggleSpeak both create a fresh Audio()
// per chunk, so this always holds); a second call on the same element throws.
export function tapElement(el) {
  const ctx = ensureAudioCtx()
  const source = ctx.createMediaElementSource(el)
  const analyser = ctx.createAnalyser()
  analyser.fftSize = 512
  analyser.smoothingTimeConstant = 0.6
  source.connect(analyser)
  source.connect(ctx.destination)
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
