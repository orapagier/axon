// On-device wake word detection ("Hey Axon") via rustpotter-worklet (WASM).
// Unlike the removed Web Speech implementation, everything runs locally: the
// mic stream stays open (steady OS indicator, no per-session chimes) and audio
// never leaves the machine. The runtime assets and the personal .rpw model are
// served from /rustpotter/ (copies of rustpotter-worklet@3.0.3 dist files —
// re-copy them if the npm package is ever upgraded).
import { RustpotterService, ScoreMode } from 'rustpotter-worklet'

const ASSETS = {
  workletPath: '/rustpotter/rustpotter-worklet.min.js',
  workerPath: '/rustpotter/rustpotter-worker.min.js',
  wasmPath: '/rustpotter/rustpotter_wasm_bg.wasm',
}
const MODEL_URL = '/rustpotter/heyaxon.rpw'
const MODEL_KEY = 'hey axon'

// Mirrors the CLI flags that passed the live mic test (spot -g -e -t 0.47):
// eager detection at 10 partial scores, gain normalization on, band-pass off.
const DETECTOR_CONFIG = {
  threshold: 0.47,
  averagedThreshold: 0,
  scoreRef: 0.22,
  bandSize: 5,
  minScores: 10,
  eager: true,
  scoreMode: ScoreMode.max,
  vadMode: undefined,
  gainNormalizerEnabled: true,
  minGain: 0.1,
  maxGain: 1.0,
  bandPassEnabled: false,
  bandPassLowCutoff: 80,
  bandPassHighCutoff: 400,
}

// Command capture bounds (silence watcher): stop after 1.4s of quiet once
// speech was heard, give up if nothing is said within 5s, hard cap at 12s.
const RMS_SPEECH = 0.012
const QUIET_TICKS = 14
const NO_SPEECH_TICKS = 50
const MAX_TICKS = 120

export const wakeWordSupported =
  typeof navigator !== 'undefined' &&
  !!navigator.mediaDevices?.getUserMedia &&
  typeof AudioWorkletNode !== 'undefined' &&
  typeof WebAssembly !== 'undefined'

export function createWakeWord({ onDetection, onState }) {
  let ctx = null
  let service = null
  let stream = null
  let source = null
  let node = null
  let analyser = null
  let running = false
  let silenceTimer = null

  function ensureCtx() {
    if (!ctx) {
      const Ctx = window.AudioContext || window.webkitAudioContext
      ctx = new Ctx()
    }
    if (ctx.state === 'suspended') ctx.resume()
    return ctx
  }

  async function start() {
    if (running) return
    try {
      ensureCtx()
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      })
      service = await RustpotterService.new(ctx.sampleRate, ASSETS, DETECTOR_CONFIG)
      service.onDetection((d) => {
        if (running) onDetection(d)
      })
      const ok = await service.addWakewordByPath(MODEL_KEY, MODEL_URL)
      if (!ok) throw new Error('wake word model rejected')
      node = await service.getProcessorNode(ctx)
      source = ctx.createMediaStreamSource(stream)
      analyser = ctx.createAnalyser()
      analyser.fftSize = 1024
      source.connect(analyser)
      source.connect(node)
      // The processor writes no output (silence); connecting it keeps the
      // audio graph alive in browsers that skip unconnected worklets.
      node.connect(ctx.destination)
      running = true
      onState('listening')
    } catch (err) {
      await teardown()
      throw err
    }
  }

  async function teardown() {
    running = false
    cancelSilenceWatch()
    try {
      source?.disconnect()
    } catch {
      // already disconnected
    }
    try {
      node?.disconnect()
    } catch {
      // already disconnected
    }
    if (service) {
      // dispose before close: close() alone leaves the worklet port dangling
      await service.disposeProcessorNode().catch(() => {})
      await service.close().catch(() => {})
    }
    stream?.getTracks().forEach((t) => t.stop())
    service = stream = source = node = analyser = null
  }

  async function stop() {
    await teardown()
    onState('off')
  }

  // Two rising sine notes, ~0.3s — "I'm listening" without shipping assets.
  // Reuses the worklet's AudioContext, which the enable click already unlocked.
  function chime() {
    try {
      const c = ensureCtx()
      const t = c.currentTime
      const osc = c.createOscillator()
      const gain = c.createGain()
      osc.type = 'sine'
      osc.frequency.setValueAtTime(660, t)
      osc.frequency.setValueAtTime(880, t + 0.1)
      gain.gain.setValueAtTime(0.0001, t)
      gain.gain.exponentialRampToValueAtTime(0.15, t + 0.02)
      gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.3)
      osc.connect(gain)
      gain.connect(c.destination)
      osc.start(t)
      osc.stop(t + 0.32)
    } catch {
      // no audio — the button state change is still visible
    }
  }

  // Watches mic level after a detection so the command recording stops itself
  // when the user finishes talking; onDone fires exactly once.
  function watchSilence(onDone) {
    cancelSilenceWatch()
    if (!analyser) return
    const data = new Float32Array(analyser.fftSize)
    let hadSpeech = false
    let quiet = 0
    let ticks = 0
    silenceTimer = setInterval(() => {
      analyser.getFloatTimeDomainData(data)
      let acc = 0
      for (let i = 0; i < data.length; i++) acc += data[i] * data[i]
      const rms = Math.sqrt(acc / data.length)
      ticks++
      if (rms > RMS_SPEECH) {
        hadSpeech = true
        quiet = 0
      } else if (hadSpeech) {
        quiet++
      }
      if ((hadSpeech && quiet >= QUIET_TICKS) || (!hadSpeech && ticks >= NO_SPEECH_TICKS) || ticks >= MAX_TICKS) {
        cancelSilenceWatch()
        onDone()
      }
    }, 100)
  }

  function cancelSilenceWatch() {
    clearInterval(silenceTimer)
    silenceTimer = null
  }

  return {
    start,
    stop,
    chime,
    watchSilence,
    cancelSilenceWatch,
    get stream() {
      return stream
    },
    get running() {
      return running
    },
  }
}
