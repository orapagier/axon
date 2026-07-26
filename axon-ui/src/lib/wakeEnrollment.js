// Wake-word enrollment capture for the dashboard: records a handful of takes
// as 16k mono PCM16 WAV — the format the backend's wake-model-builder (and
// axon-voice's own on-device JNI builder) expect via rustpotter's WAV reader.
// Deliberately skips echoCancellation/noiseSuppression/autoGainControl,
// mirroring axon-voice's WavRecorder(cleanSignal=true): those effects strip
// the quiet signal a wake model gets matched against.

const SAMPLE_RATE = 16000

/** Wrap 16-bit PCM samples in a standard 44-byte WAV header. */
export function encodeWav(float32, sampleRate = SAMPLE_RATE) {
  const pcm = new Int16Array(float32.length)
  for (let i = 0; i < float32.length; i++) {
    const s = Math.max(-1, Math.min(1, float32[i]))
    pcm[i] = s < 0 ? s * 0x8000 : s * 0x7fff
  }
  const byteRate = sampleRate * 2
  const buffer = new ArrayBuffer(44 + pcm.length * 2)
  const view = new DataView(buffer)
  const writeStr = (offset, str) => {
    for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i))
  }
  writeStr(0, 'RIFF')
  view.setUint32(4, 36 + pcm.length * 2, true)
  writeStr(8, 'WAVE')
  writeStr(12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true) // PCM
  view.setUint16(22, 1, true) // mono
  view.setUint32(24, sampleRate, true)
  view.setUint32(28, byteRate, true)
  view.setUint16(32, 2, true) // block align
  view.setUint16(34, 16, true) // bits per sample
  writeStr(36, 'data')
  view.setUint32(40, pcm.length * 2, true)
  new Int16Array(buffer, 44).set(pcm)
  return new Blob([buffer], { type: 'audio/wav' })
}

/** One-take-at-a-time recorder. start() begins capture and reports RMS level
 *  (0..1) via onLevel roughly every audio callback; stop() tears the graph
 *  down and returns the take as a WAV Blob. */
export function createTakeRecorder() {
  let ctx = null
  let stream = null
  let source = null
  let proc = null
  let silence = null
  let chunks = []

  async function start(onLevel) {
    chunks = []
    const Ctx = window.AudioContext || window.webkitAudioContext
    // Best-effort — not every browser honors the constructor's sampleRate
    // hint, so the actual encode rate below always reads back ctx.sampleRate.
    ctx = new Ctx({ sampleRate: SAMPLE_RATE })
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
    })
    source = ctx.createMediaStreamSource(stream)
    proc = ctx.createScriptProcessor(4096, 1, 1)
    // ScriptProcessorNode only fires onaudioprocess once connected through to
    // a destination in some browsers; a zero-gain node keeps the captured
    // audio from actually being played back.
    silence = ctx.createGain()
    silence.gain.value = 0
    proc.onaudioprocess = (e) => {
      const data = e.inputBuffer.getChannelData(0)
      chunks.push(new Float32Array(data))
      let acc = 0
      for (let i = 0; i < data.length; i++) acc += data[i] * data[i]
      onLevel(Math.sqrt(acc / data.length))
    }
    source.connect(proc)
    proc.connect(silence)
    silence.connect(ctx.destination)
  }

  function stop() {
    source?.disconnect()
    proc?.disconnect()
    silence?.disconnect()
    stream?.getTracks().forEach((t) => t.stop())
    const total = chunks.reduce((n, c) => n + c.length, 0)
    const merged = new Float32Array(total)
    let off = 0
    for (const c of chunks) {
      merged.set(c, off)
      off += c.length
    }
    chunks = []
    const rate = ctx?.sampleRate || SAMPLE_RATE
    ctx?.close().catch(() => {})
    ctx = stream = source = proc = silence = null
    return encodeWav(merged, rate)
  }

  return { start, stop }
}
