// Amplitude sources for the hands-free voice orb (VoiceOrb.vue), all on the
// same roughly-0..1 RMS scale.
//
//  - readLevel(analyser): live RMS of an AnalyserNode. The only one fed through
//    here is the wake mic's own (wakeword.js) during the 'listening' phase — a
//    passive read of a node that already exists for silence detection, so it
//    never touches audio playback.
//
//  - buildTtsEnvelope(blob): a precomputed RMS envelope of a reply's audio for
//    the 'speaking' phase, which the orb samples by the <audio> element's
//    currentTime. This deliberately does NOT route the playing element through
//    a Web Audio graph: both createMediaElementSource (reroutes the element,
//    needs a manual reconnect to destination) and captureStream() proved able
//    to silently mute the reply when the audio context isn't confirmed-running
//    — the "orb animates but nothing plays" bug that got element tapping
//    removed. Decoding the bytes in a separate context that never connects to
//    any output cannot affect playback at all, and sampling by currentTime
//    keeps the orb in sync with what is actually audible.

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

// A context used ONLY to decode TTS bytes into an amplitude envelope. It is
// never resumed and never connected to an output, so it neither needs an
// autoplay-unlock gesture nor can it mute the reply the <audio> element plays.
let decodeCtx = null
function ensureDecodeCtx() {
  if (!decodeCtx) {
    const Ctx = window.AudioContext || window.webkitAudioContext
    if (!Ctx) return null
    decodeCtx = new Ctx()
  }
  return decodeCtx
}

const HOP_SEC = 0.02 // 20ms envelope resolution — roughly one orb frame

// Decode `blob` and return { duration, level(timeSec) -> 0..1 }: an RMS
// envelope the orb samples at the element's currentTime. Returns null if the
// audio can't be decoded (unsupported format, no Web Audio) — the caller then
// falls back to the synthetic talking envelope.
export async function buildTtsEnvelope(blob) {
  try {
    const ctx = ensureDecodeCtx()
    if (!ctx) return null
    const bytes = await blob.arrayBuffer()
    const audio = await ctx.decodeAudioData(bytes)
    const hop = Math.max(1, Math.round(audio.sampleRate * HOP_SEC))
    const bins = Math.max(1, Math.ceil(audio.length / hop))
    const env = new Float32Array(bins) // sum of per-channel mean-square, then RMS
    const channels = audio.numberOfChannels
    for (let c = 0; c < channels; c++) {
      const data = audio.getChannelData(c)
      for (let b = 0; b < bins; b++) {
        const start = b * hop
        const end = Math.min(audio.length, start + hop)
        let acc = 0
        for (let i = start; i < end; i++) acc += data[i] * data[i]
        env[b] += end > start ? acc / (end - start) : 0
      }
    }
    for (let b = 0; b < bins; b++) env[b] = Math.sqrt(env[b] / channels)
    return {
      duration: audio.duration,
      level(timeSec) {
        if (!(timeSec >= 0)) return 0
        const b = Math.floor(timeSec / HOP_SEC)
        return b >= 0 && b < bins ? env[b] : 0
      },
    }
  } catch {
    return null // undecodable — orb uses its synthetic envelope
  }
}
