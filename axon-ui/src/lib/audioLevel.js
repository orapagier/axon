// Reads a live AnalyserNode's amplitude for the hands-free voice orb
// (VoiceOrb.vue). The only analyser fed through here is the wake mic's own
// (wakeword.js) during the 'listening' phase — a passive read of a node that
// already exists for silence detection, so it never touches audio playback.
//
// The reply ('speaking') phase intentionally has NO tap: routing a playing
// <audio> element through a Web Audio graph to meter it can silently mute the
// output when the audio context isn't in a confirmed-running state. The orb
// uses a synthetic talking envelope there instead, keeping the visualization
// completely decoupled from the audio pipeline.
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
