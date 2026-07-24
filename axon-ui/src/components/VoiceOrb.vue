<script setup>
// A JARVIS-style reactive orb for the hands-free ("Hey Axon") overlay in
// ChatPage.vue. Draws to a <canvas> on its own rAF loop rather than through
// Vue's reactivity — the animation needs a fresh frame ~60x/sec regardless of
// whether any prop actually changed, which is a poor fit for template
// re-renders.
//
// [analyser] is the live Web Audio AnalyserNode to react to — the wake mic's
// during 'listening'. During 'speaking', [speakSample] returns the reply
// audio's RMS at the current playback position (a decoded envelope sampled by
// the <audio> element's currentTime — see lib/audioLevel.js; no Web Audio graph
// touches the playing element, so it can't mute the reply). Both are absent
// during 'thinking', and 'speaking' also has no sample when TTS fell back to
// the browser's speechSynthesis (no decodable bytes) — synthesizeLevel() fills
// in a plausible "talking" envelope so the orb stays alive instead of freezing.
import { onMounted, onUnmounted, ref } from 'vue'
import { readLevel } from '../lib/audioLevel.js'

// Metered RMS is small; these lift it into a lively 0..1 orb range. The mic is
// quiet (speech ~0.02–0.08) so it needs more gain than the cleaner, louder
// reply audio. Both empirical.
const MIC_GAIN = 9
const SPEAK_GAIN = 4

const props = defineProps({
  phase: { type: String, default: 'listening' }, // 'listening' | 'thinking' | 'speaking'
  analyser: { type: Object, default: null },
  speakSample: { type: Function, default: null }, // () => reply RMS 0..1, or null
})

const canvasEl = ref(null)
let raf = 0
let ctx2d = null
let t = 0
let smoothed = 0
let colors = { accent: '#4dabf7', glow: '#22d3ee', bg: '#0d1017' }

function synthesizeLevel() {
  if (props.phase === 'speaking') {
    // Layered sines beat against each other into something syllable-shaped
    // rather than a flat metronomic pulse.
    return 0.22 + 0.22 * Math.abs(Math.sin(t * 5.3)) * (0.55 + 0.45 * Math.sin(t * 1.4))
  }
  if (props.phase === 'thinking') return 0.12 + 0.05 * Math.sin(t * 1.6)
  return 0.08 // listening with no analyser yet (first frame or two)
}

function draw() {
  const canvas = canvasEl.value
  if (!canvas || !ctx2d) return
  const w = canvas.clientWidth
  const h = canvas.clientHeight
  const cx = w / 2
  const cy = h / 2
  t += 1 / 60

  // Real metered level for 'listening' (mic) and 'speaking' (reply audio);
  // synth for 'thinking' and any un-metered gap (browser-voice fallback, or
  // before the first sample of a sentence arrives).
  let raw
  if (props.phase === 'speaking' && props.speakSample) {
    const v = props.speakSample()
    raw = v == null ? synthesizeLevel() : Math.min(1, v * SPEAK_GAIN)
  } else if (props.analyser) {
    raw = Math.min(1, readLevel(props.analyser) * MIC_GAIN)
  } else {
    raw = synthesizeLevel()
  }
  // Snap up fast (speech onset should feel immediate), decay slower (a
  // silence gap between words shouldn't collapse the orb to nothing).
  smoothed += (raw - smoothed) * (raw > smoothed ? 0.5 : 0.12)

  ctx2d.clearRect(0, 0, w, h)

  const baseR = Math.min(w, h) * 0.16
  const coreR = baseR * (1 + smoothed * 0.9)

  // Ambient rotating ring reads as "processing" even at a near-zero level.
  ctx2d.save()
  ctx2d.translate(cx, cy)
  ctx2d.rotate(t * (props.phase === 'thinking' ? 0.6 : 0.25))
  ctx2d.strokeStyle = colors.glow
  ctx2d.globalAlpha = 0.25
  ctx2d.lineWidth = 2
  ctx2d.setLineDash([6, 10])
  ctx2d.beginPath()
  ctx2d.arc(0, 0, baseR * 2.3, 0, Math.PI * 2)
  ctx2d.stroke()
  ctx2d.restore()

  // Level-reactive rings, staggered outward.
  ctx2d.setLineDash([])
  for (let i = 0; i < 3; i++) {
    const spread = baseR * (1.3 + i * 0.45) + smoothed * baseR * (1.6 + i * 0.5)
    ctx2d.beginPath()
    ctx2d.strokeStyle = i === 0 ? colors.accent : colors.glow
    ctx2d.globalAlpha = Math.max(0, 0.35 - i * 0.1) * (0.4 + smoothed)
    ctx2d.lineWidth = 1.5
    ctx2d.arc(cx, cy, spread, 0, Math.PI * 2)
    ctx2d.stroke()
  }

  // Core glow.
  const grad = ctx2d.createRadialGradient(cx, cy, 0, cx, cy, coreR * 1.8)
  grad.addColorStop(0, colors.glow)
  grad.addColorStop(0.55, colors.accent)
  grad.addColorStop(1, 'transparent')
  ctx2d.globalAlpha = 1
  ctx2d.fillStyle = grad
  ctx2d.beginPath()
  ctx2d.arc(cx, cy, coreR * 1.8, 0, Math.PI * 2)
  ctx2d.fill()

  ctx2d.fillStyle = colors.bg
  ctx2d.globalAlpha = 0.85
  ctx2d.beginPath()
  ctx2d.arc(cx, cy, coreR * 0.55, 0, Math.PI * 2)
  ctx2d.fill()

  raf = requestAnimationFrame(draw)
}

function readCssColors() {
  const s = getComputedStyle(document.documentElement)
  colors = {
    accent: s.getPropertyValue('--accent').trim() || colors.accent,
    glow: s.getPropertyValue('--accent-glow').trim() || colors.glow,
    bg: s.getPropertyValue('--bg').trim() || colors.bg,
  }
}

// Backing store sized for devicePixelRatio so the orb stays crisp; scale()
// composes with that pixel buffer so the draw math above stays in CSS-pixel
// units no matter the display density.
function setupCanvas() {
  const canvas = canvasEl.value
  if (!canvas) return
  const rect = canvas.getBoundingClientRect()
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.round(rect.width * dpr)
  canvas.height = Math.round(rect.height * dpr)
  ctx2d = canvas.getContext('2d')
  ctx2d.scale(dpr, dpr)
}

onMounted(() => {
  readCssColors()
  setupCanvas()
  raf = requestAnimationFrame(draw)
})

onUnmounted(() => {
  cancelAnimationFrame(raf)
})
</script>

<template>
  <div class="voice-orb">
    <canvas
      ref="canvasEl"
      class="voice-orb-canvas"
      aria-hidden="true"
    />
  </div>
</template>

<style scoped>
.voice-orb {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 280px;
  height: 280px;
  max-width: 60vw;
  max-height: 60vw;
}

.voice-orb-canvas {
  width: 100%;
  height: 100%;
}
</style>
