<script setup>
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { connectWs, wsSend, wsStatus } from '../lib/ws.js'
import { get, put, del, postForm, postRaw } from '../lib/api.js'
import { toast, notifyBell } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { renderMarkdown } from '../lib/markdown.js'
import SearchInput from '../components/SearchInput.vue'

// Each message: { role:'user'|'agent'|'trace', text, meta?, trace:[], thinking?:boolean }
const messages = ref([])
const input = ref('')
const disabled = ref(false)
const messagesEl = ref(null)
const inputEl = ref(null)
const starterPrompts = [
  'Summarize my connected services and tell me what is missing.',
  'Help me design a workflow for inbound lead qualification.',
  'Review my model setup and suggest a cleaner routing strategy.',
]

// Current in-flight agent response
let currentRunId = null
let agentIdx = -1 // index in messages[] of the in-progress agent msg
let traceIdx = -1 // index of the trace block preceding it

// Conversation threads (ChatGPT-style). Each thread has its own session_id, so
// the agent only sees that thread's history; long-term memory stays shared.
const conversations = ref([])
const currentSessionId = ref(null)

// Chat-history search (over message content, not just titles). Debounced
// against the /conversations/search endpoint; an empty query restores the
// normal newest-first sidebar list.
const historySearch = ref('')
const historyResults = ref(null) // null = not searching; [] = searching, no matches
let historySearchTimer = null

const sidebarConversations = computed(() => (historyResults.value !== null ? historyResults.value : conversations.value))

watch(historySearch, (q) => {
  clearTimeout(historySearchTimer)
  const trimmed = q.trim()
  if (!trimmed) {
    historyResults.value = null
    return
  }
  historySearchTimer = setTimeout(async () => {
    try {
      const res = await get(`/conversations/search?q=${encodeURIComponent(trimmed)}`)
      historyResults.value = res.conversations || []
    } catch {
      historyResults.value = []
    }
  }, 300)
})

// Splits a snippet like "…before <mark>match</mark> after…" into plain-text
// and highlighted segments so it can be rendered without v-html (message
// content is user-typed and must stay escaped even inside a highlight).
function highlightSegments(snippet) {
  if (!snippet) return []
  return snippet
    .split(/(<mark>.*?<\/mark>)/g)
    .filter(Boolean)
    .map((part) => {
      const m = part.match(/^<mark>([\s\S]*)<\/mark>$/)
      return m ? { text: m[1], mark: true } : { text: part, mark: false }
    })
}

// Inline rename state: the conversation id currently being edited + its draft.
const renamingId = ref(null)
const renameText = ref('')
const renameEl = ref(null)

// crypto.randomUUID needs a secure context (https/localhost); fall back to a
// v4 generator so plain-http dashboards still get unique ids.
function uuid() {
  if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID()
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0
    const v = c === 'x' ? r : (r & 0x3) | 0x8
    return v.toString(16)
  })
}

function rowToMessage(m) {
  if (m.role === 'trace') {
    // Persisted reasoning trace — rehydrated collapsed to save space.
    return { role: 'trace', trace: Array.isArray(m.items) ? m.items : [], collapsed: true }
  }
  return m.role === 'assistant'
    ? { role: 'agent', text: m.content, thinking: false, meta: '', status: '' }
    : { role: 'user', text: m.content }
}

// Collapse the in-flight trace block once its run is over; it stays available
// behind the "Reasoning" toggle instead of taking up transcript space.
function collapseTrace() {
  if (traceIdx >= 0 && messages.value[traceIdx]) {
    messages.value[traceIdx].collapsed = true
  }
}

function resetRunTrackers() {
  currentRunId = null
  agentIdx = -1
  traceIdx = -1
}

async function loadConversations() {
  try {
    const res = await get('/conversations')
    conversations.value = res.conversations || []
  } catch {
    /* sidebar is best-effort; leave the list as-is on failure */
  }
}

// Convenience autofocus (on mount, new chat, run finish) is desktop-only:
// on touch devices a programmatic focus pops the on-screen keyboard
// uninvited — native chat apps never open the keyboard on their own.
const AUTOFOCUS_OK = !window.matchMedia('(pointer: coarse)').matches
function focusComposer() {
  if (AUTOFOCUS_OK) inputEl.value?.focus()
}

function newChat() {
  currentSessionId.value = uuid()
  messages.value = []
  resetRunTrackers()
  stopSpeaking()
  disabled.value = false
  nextTick(() => focusComposer())
}

async function openConversation(id) {
  if (id === currentSessionId.value || disabled.value) return
  currentSessionId.value = id
  resetRunTrackers()
  stopSpeaking()
  try {
    const res = await get(`/conversations/${id}/messages`)
    messages.value = (res.messages || []).map(rowToMessage)
  } catch {
    messages.value = []
  }
  scrollBottom()
}

async function removeConversation(id) {
  const ok = await confirmDialog('This conversation and its messages will be permanently deleted. This action cannot be undone.', {
    title: 'Delete Conversation',
    confirmText: 'Delete',
  })
  if (!ok) return
  try {
    await del(`/conversations/${id}`)
  } catch {
    toast('Failed to delete conversation', false)
    return
  }
  if (id === currentSessionId.value) newChat()
  loadConversations()
}

function startRename(c) {
  renamingId.value = c.id
  renameText.value = c.title || ''
  nextTick(() => {
    // refs inside v-for collect into an array; fall back to a bare ref.
    const el = Array.isArray(renameEl.value) ? renameEl.value[0] : renameEl.value
    el?.focus()
    el?.select()
  })
}

function cancelRename() {
  renamingId.value = null
  renameText.value = ''
}

async function commitRename(c) {
  if (renamingId.value !== c.id) return // already committed/cancelled
  const title = renameText.value.trim()
  renamingId.value = null
  if (!title || title === c.title) return
  c.title = title // optimistic
  try {
    await put(`/conversations/${c.id}`, { title })
  } catch {
    toast('Failed to rename conversation', false)
    loadConversations()
  }
}

function prettyStatus(text) {
  if (!text) return 'Thinking...'
  if (text.startsWith('Iteration ')) return 'Working on it...'
  return text
}

function handleWsEvent(ev) {
  if (!currentRunId && ev.run_id) currentRunId = ev.run_id

  switch (ev.type) {
    case 'thinking':
      if (ev.run_id !== currentRunId) break
      if (traceIdx >= 0) {
        messages.value[traceIdx].trace.push({ text: `... ${ev.text}`, color: '#98a6a1' })
      }
      if (agentIdx >= 0 && messages.value[agentIdx].thinking) {
        messages.value[agentIdx].status = prettyStatus(ev.text)
      }
      break

    case 'model': {
      if (ev.run_id !== currentRunId) break
      const dur = ev.duration_ms ? ` (${ev.duration_ms}ms)` : ''
      if (traceIdx >= 0) {
        messages.value[traceIdx].trace.push({
          text: `Model ${ev.model} iter ${ev.iteration}${dur}`,
          color: '#d7e7bc',
        })
      }
      if (agentIdx >= 0 && messages.value[agentIdx].thinking) {
        messages.value[agentIdx].status = `Model ${ev.model} responded`
      }
      break
    }

    case 'tools': {
      if (ev.run_id !== currentRunId) break
      const par = ev.parallel ? 'parallel' : 'sequential'
      if (traceIdx >= 0) {
        messages.value[traceIdx].trace.push({
          text: `Tools ${ev.tier} -> [${(ev.tools || []).join(', ')}] ${par}`,
          color: '#b5cbc6',
        })
      }
      if (agentIdx >= 0 && messages.value[agentIdx].thinking && (ev.tools || []).length) {
        messages.value[agentIdx].status = `Planning with ${(ev.tools || []).length} tool${(ev.tools || []).length > 1 ? 's' : ''}...`
      }
      break
    }

    case 'tool_start':
      if (ev.run_id !== currentRunId) break
      if (traceIdx >= 0) {
        messages.value[traceIdx].trace.push({
          id: ev.tool_call_id,
          text: `Start ${ev.tool}...`,
          color: '#d9c187',
        })
      }
      if (agentIdx >= 0 && messages.value[agentIdx].thinking) {
        messages.value[agentIdx].status = `Using ${ev.tool.replaceAll('_', ' ')}...`
      }
      break

    case 'tool_end':
      if (ev.run_id !== currentRunId) break
      if (traceIdx >= 0) {
        const items = messages.value[traceIdx].trace
        const i = items.findIndex((x) => x.id === ev.tool_call_id)
        if (i >= 0) {
          items[i] = {
            ...items[i],
            text: `${ev.ok ? 'OK' : 'ERR'} ${ev.tool} ${ev.duration_ms}ms`,
            color: ev.ok ? '#b7d79a' : '#e4a1a1',
          }
        } else {
          items.push({
            text: `${ev.ok ? 'OK' : 'ERR'} ${ev.tool} ${ev.duration_ms}ms`,
            color: ev.ok ? '#b7d79a' : '#e4a1a1',
          })
        }
      }
      if (agentIdx >= 0 && messages.value[agentIdx].thinking) {
        messages.value[agentIdx].status = ev.ok ? 'Processing tool results...' : `Recovering from ${ev.tool} error...`
      }
      break

    case 'token':
      if (ev.run_id !== currentRunId) break
      if (agentIdx >= 0) {
        messages.value[agentIdx].thinking = false
        messages.value[agentIdx].status = ''
        messages.value[agentIdx].text += ev.text
        scrollBottom()
      }
      break

    case 'memory_hit':
      if (ev.run_id !== currentRunId) break
      if (traceIdx >= 0) {
        messages.value[traceIdx].trace.push({ text: `${ev.count} memories retrieved`, color: '#b5cbc6' })
      }
      break

    case 'notification': {
      if (ev.run_id && currentRunId && ev.run_id !== currentRunId) break
      const title = (ev.title || '').trim()
      const message = (ev.message || '').trim()
      const body = title ? `${title}\n${message}` : (message || 'Notification')
      const ok = (ev.level || '').toLowerCase() !== 'error'
      // Backend-pushed notifications (watchers, background jobs) are
      // review-worthy: record in the bell, not just a transient toast.
      notifyBell(body, ok)
      break
    }

    case 'done':
      if (ev.run_id !== currentRunId) break
      if (agentIdx >= 0) {
        messages.value[agentIdx].thinking = false
        messages.value[agentIdx].status = ''
        if (!messages.value[agentIdx].text && ev.full_text) {
          messages.value[agentIdx].text = ev.full_text
        }
        const dur = ev.total_duration_ms ? ` | ${ev.total_duration_ms}ms` : ''
        messages.value[agentIdx].meta = `${ev.iterations} iter | ${ev.total_tokens} tokens${dur}`
      }
      if (speakReplyOnDone) {
        speakReplyOnDone = false
        if (agentIdx >= 0 && canSpeak) toggleSpeak(agentIdx)
      }
      collapseTrace()
      resetRunTrackers()
      disabled.value = false
      // Reconcile the sidebar: a brand-new thread now has a backend title, and
      // the active thread bubbles to the top by updated_at.
      loadConversations()
      break

    case 'error':
      if (ev.run_id !== currentRunId) break
      if (agentIdx >= 0) {
        messages.value[agentIdx].thinking = false
        messages.value[agentIdx].status = ''
      }
      // Run failures (model router exhaustion, agent errors) need review —
      // keep them in the bell as well as flashing a toast.
      notifyBell(ev.message || 'Agent error', false)
      speakReplyOnDone = false
      collapseTrace()
      resetRunTrackers()
      disabled.value = false
      break
  }
}

async function scrollBottom() {
  await nextTick()
  if (messagesEl.value) {
    messagesEl.value.scrollTop = messagesEl.value.scrollHeight
  }
}

function adjustInputHeight() {
  if (!inputEl.value) return
  inputEl.value.style.height = 'auto'
  inputEl.value.style.height = `${Math.min(inputEl.value.scrollHeight, 160)}px`
}

function useStarterPrompt(prompt) {
  input.value = prompt
  nextTick(() => {
    adjustInputHeight()
    inputEl.value?.focus()
  })
}

async function send() {
  const msg = input.value.trim()
  if (!msg || disabled.value) return
  speakReplyOnDone = voiceSendPending
  voiceSendPending = false
  if (!currentSessionId.value) newChat()

  messages.value.push({ role: 'user', text: msg })
  input.value = ''
  disabled.value = true
  adjustInputHeight()

  // Add trace block (expanded while the run streams) then agent bubble
  messages.value.push({ role: 'trace', trace: [], collapsed: false })
  messages.value.push({ role: 'agent', text: '', thinking: true, meta: '', status: 'Thinking...' })

  traceIdx = messages.value.length - 2
  agentIdx = messages.value.length - 1

  // Optimistically surface a brand-new thread in the sidebar right away; the
  // 'done' handler reconciles it with the server's title/order.
  if (!conversations.value.some((c) => c.id === currentSessionId.value)) {
    conversations.value.unshift({
      id: currentSessionId.value,
      title: msg.slice(0, 60) || 'New chat',
      updated_at: '',
    })
  }

  await scrollBottom()
  const sent = wsSend({ task: msg, session_id: currentSessionId.value })
  if (!sent) {
    // Socket is down — undo the placeholders and give the message back
    // instead of dropping it silently and locking the input forever.
    messages.value.splice(traceIdx, 2)
    traceIdx = -1
    agentIdx = -1
    disabled.value = false
    input.value = msg
    toast('Not connected to the agent yet — retry once the status shows Connected.', false)
  }
}

// Abort the in-flight run: tell the backend to cancel it and unlock the input
// immediately. Late token/done events are ignored because agentIdx is reset to
// -1 (every message mutation in handleWsEvent is guarded by agentIdx >= 0).
function stop() {
  if (!disabled.value) return
  wsSend({ type: 'cancel', session_id: currentSessionId.value })
  if (agentIdx >= 0) {
    const m = messages.value[agentIdx]
    m.thinking = false
    m.status = ''
    if (!m.text) m.text = 'Stopped.'
    m.meta = m.meta ? `${m.meta} · stopped` : 'stopped'
  }
  speakReplyOnDone = false
  collapseTrace()
  resetRunTrackers()
  disabled.value = false
}

function onKeydown(e) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    send()
  }
}

// ── Voice input (mic → /api/audio/transcribe → send) ────────────────────────
// One button cycles idle → recording → transcribing → idle. The transcript
// auto-sends (speak-and-go, like the messaging gateways); it only stays in the
// composer when a run is already streaming and sending is blocked.
const recState = ref('idle') // 'idle' | 'recording' | 'transcribing'
const recSeconds = ref(0)
let mediaRecorder = null
let recChunks = []
let recTimer = null
let recCancelled = false
// Voice round trip: a mic-initiated send marks the run so its reply is read
// aloud on 'done'; typed sends never are. One run at a time (disabled gate),
// so a single pair of flags is enough.
let voiceSendPending = false
let speakReplyOnDone = false

// getUserMedia needs a secure context (https/localhost); hide the mic instead
// of showing a button that can only fail.
const micSupported =
  typeof navigator !== 'undefined' &&
  !!navigator.mediaDevices?.getUserMedia &&
  typeof MediaRecorder !== 'undefined'

const recClock = computed(() => {
  const m = Math.floor(recSeconds.value / 60)
  const s = String(recSeconds.value % 60).padStart(2, '0')
  return `${m}:${s}`
})

function recorderMime() {
  // Chrome/Firefox/Edge produce webm/opus; Safari only mp4. Whisper-style
  // endpoints accept both — the container is signaled via the upload filename.
  return (
    ['audio/webm;codecs=opus', 'audio/webm', 'audio/mp4'].find((m) =>
      MediaRecorder.isTypeSupported(m)
    ) || ''
  )
}

async function startRecording() {
  if (recState.value !== 'idle' || disabled.value) return
  let stream
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true })
  } catch {
    toast('Microphone access was denied — allow it for this site and try again.', false)
    return
  }
  const mime = recorderMime()
  try {
    mediaRecorder = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream)
  } catch {
    stream.getTracks().forEach((t) => t.stop())
    toast('Audio recording is not supported in this browser.', false)
    return
  }
  recChunks = []
  recCancelled = false
  mediaRecorder.ondataavailable = (e) => {
    if (e.data && e.data.size > 0) recChunks.push(e.data)
  }
  mediaRecorder.onstop = () => {
    stream.getTracks().forEach((t) => t.stop())
    clearInterval(recTimer)
    const blob = new Blob(recChunks, { type: mediaRecorder.mimeType || 'audio/webm' })
    recChunks = []
    mediaRecorder = null
    // A sub-kilobyte blob is a stray click, not speech — drop it silently.
    if (recCancelled || blob.size < 1024) {
      recState.value = 'idle'
      recSeconds.value = 0
      return
    }
    transcribe(blob)
  }
  recSeconds.value = 0
  recTimer = setInterval(() => {
    recSeconds.value += 1
  }, 1000)
  mediaRecorder.start()
  recState.value = 'recording'
}

function stopRecording(cancel = false) {
  if (recState.value !== 'recording' || !mediaRecorder) return
  recCancelled = cancel
  mediaRecorder.stop() // onstop handles cleanup + the next state
}

async function transcribe(blob) {
  recState.value = 'transcribing'
  const ext = blob.type.includes('mp4') ? 'mp4' : blob.type.includes('ogg') ? 'ogg' : 'webm'
  const fd = new FormData()
  fd.append('file', blob, `recording.${ext}`)
  try {
    const res = await postForm('/audio/transcribe', fd)
    const text = (res.text || '').trim()
    if (res.error) {
      toast(res.error, false)
    } else if (!text) {
      toast('No speech detected in the recording.', false)
    } else {
      // Append rather than replace: dictation can extend typed text.
      input.value = input.value.trim() ? `${input.value.replace(/\s+$/, '')} ${text}` : text
      if (!disabled.value) {
        voiceSendPending = true
        send()
      } else {
        // A run is streaming — sending is blocked, so keep it for review.
        nextTick(() => {
          adjustInputHeight()
          inputEl.value?.focus()
        })
      }
    }
  } catch {
    toast('Transcription failed — check the Voice Input settings.', false)
  } finally {
    recState.value = 'idle'
    recSeconds.value = 0
  }
}

// ── Read aloud (server TTS first, browser speech synthesis fallback) ────────
// toggleSpeak tries the configured tts.* endpoint (POST /audio/speech → audio
// blob → playback); when TTS is unconfigured (503), the provider errors or
// rate-limits (502), the network fails, or autoplay is blocked, it falls back
// to the browser's built-in speechSynthesis — the original zero-config path.
const ttsSupported = typeof window !== 'undefined' && 'speechSynthesis' in window
const audioSupported = typeof Audio !== 'undefined'
const canSpeak = ttsSupported || audioSupported
const speakingIdx = ref(-1)
let speakSeq = 0 // bumping this invalidates any in-flight synthesis
let speakAbort = null // aborts the in-flight /audio/speech fetch on stop
let audioEl = null
let audioUrl = null

// The agent bubble renders markdown; the utterance needs the prose only.
function plainTextForSpeech(md) {
  return String(md || '')
    .replace(/```[\s\S]*?```/g, ' Code block omitted. ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, '')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/[*_~>#]/g, '')
    .replace(/\s+/g, ' ')
    .trim()
}

function releaseAudio() {
  if (audioEl) {
    audioEl.onended = null
    audioEl.onerror = null
    audioEl.pause()
    audioEl = null
  }
  if (audioUrl) {
    URL.revokeObjectURL(audioUrl)
    audioUrl = null
  }
}

function stopSpeaking() {
  speakSeq += 1
  if (speakAbort) {
    speakAbort.abort()
    speakAbort = null
  }
  releaseAudio()
  if (ttsSupported) window.speechSynthesis.cancel()
  speakingIdx.value = -1
}

// Today's zero-config path, now the fallback: the browser's built-in voice.
function speakWithSynthesis(idx, text) {
  if (!ttsSupported) {
    if (speakingIdx.value === idx) speakingIdx.value = -1
    return
  }
  window.speechSynthesis.cancel()
  const u = new SpeechSynthesisUtterance(text)
  u.onend = () => {
    if (speakingIdx.value === idx) speakingIdx.value = -1
  }
  u.onerror = () => {
    if (speakingIdx.value === idx) speakingIdx.value = -1
  }
  window.speechSynthesis.speak(u)
}

async function toggleSpeak(idx) {
  if (!canSpeak) return
  if (speakingIdx.value === idx) {
    stopSpeaking()
    return
  }
  stopSpeaking()
  const text = plainTextForSpeech(messages.value[idx]?.text)
  if (!text) return
  const seq = speakSeq
  speakingIdx.value = idx

  if (audioSupported) {
    try {
      speakAbort = new AbortController()
      const res = await postRaw('/audio/speech', { text }, speakAbort.signal)
      const type = res.headers.get('content-type') || ''
      if (res.ok && type.startsWith('audio/')) {
        const blob = await res.blob()
        if (seq !== speakSeq) return // stopped while synthesizing
        audioUrl = URL.createObjectURL(blob)
        audioEl = new Audio(audioUrl)
        audioEl.onended = audioEl.onerror = () => {
          if (speakingIdx.value === idx) stopSpeaking()
        }
        await audioEl.play()
        return
      }
    } catch {
      // Aborted, network failure, or blocked autoplay — clean up whatever the
      // attempt allocated; the seq guard below decides whether to fall back.
      if (seq === speakSeq) releaseAudio()
    }
    if (seq !== speakSeq) return // user hit stop during the attempt
  }
  speakWithSynthesis(idx, text)
}

// ── Wake word ("Hey Axon") ──────────────────────────────────────────────────
// Siri-style hands-free trigger: while the toggle is on, a continuous
// SpeechRecognition session watches for "hey/hi/ok Axon". The command can ride
// in the same utterance ("hey axon what's on my calendar") or follow the
// chime; either way it goes through the voice-send path so the reply is read
// aloud. Browsers cannot capture audio from a backgrounded tab or a locked
// phone, so this is foreground-only by design — the recognizer stops when the
// tab hides and restarts when it returns.
const SpeechRecognitionImpl =
  typeof window !== 'undefined' ? window.SpeechRecognition || window.webkitSpeechRecognition : null
const wakeSupported = !!SpeechRecognitionImpl
const wakeEnabled = ref(wakeSupported && localStorage.getItem('axon-wake-word') === '1')
const wakeState = ref('off') // 'off' | 'listening' | 'capturing'
const wakeHeard = ref('') // live interim transcript shown while capturing
let wakeRec = null
let wakeRestartTimer = null
let wakeCaptureTimer = null
let wakeChimeCtx = null

// "Axon" is short and the recognizer often renders it as a near-homophone, so
// a greeting prefix is required (bare "axon" mid-sentence must not trigger)
// and common mishearings of the name are accepted.
const wakeRe = /\b(?:hey|hi|ok|okay)[\s,.!]+(?:axon|axion|axen|axone|exon|action)\b[\s,.!?]*/i

// A live run, the push-to-talk recorder, or read-aloud playback all disqualify
// a trigger — without the speaking guard the assistant can wake itself off its
// own voice coming out of the speakers.
function wakeBusy() {
  return disabled.value || recState.value !== 'idle' || speakingIdx.value >= 0
}

function ensureChimeCtx() {
  if (!wakeChimeCtx) {
    const Ctx = window.AudioContext || window.webkitAudioContext
    if (Ctx) wakeChimeCtx = new Ctx()
  }
  if (wakeChimeCtx?.state === 'suspended') wakeChimeCtx.resume()
  return wakeChimeCtx
}

// Two rising sine notes, ~0.3s — says "I'm listening" without shipping assets.
function wakeChime() {
  try {
    const ctx = ensureChimeCtx()
    if (!ctx) return
    const t = ctx.currentTime
    const osc = ctx.createOscillator()
    const gain = ctx.createGain()
    osc.type = 'sine'
    osc.frequency.setValueAtTime(660, t)
    osc.frequency.setValueAtTime(880, t + 0.1)
    gain.gain.setValueAtTime(0.0001, t)
    gain.gain.exponentialRampToValueAtTime(0.15, t + 0.02)
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.3)
    osc.connect(gain)
    gain.connect(ctx.destination)
    osc.start(t)
    osc.stop(t + 0.32)
  } catch {
    // No audio — the wake button's state change is still visible.
  }
}

function sendWakeCommand(text) {
  // Same post-transcript behavior as push-to-talk: append to any draft, send
  // through the voice path so the reply is spoken, hold it if a run streams.
  input.value = input.value.trim() ? `${input.value.replace(/\s+$/, '')} ${text}` : text
  if (!disabled.value) {
    voiceSendPending = true
    send()
  } else {
    nextTick(() => adjustInputHeight())
  }
}

function cancelWakeCapture() {
  clearTimeout(wakeCaptureTimer)
  if (wakeState.value === 'capturing') wakeState.value = 'listening'
  wakeHeard.value = ''
}

function onWakeResult(e) {
  let finalText = ''
  let interim = ''
  for (let i = e.resultIndex; i < e.results.length; i++) {
    const t = e.results[i][0]?.transcript || ''
    if (e.results[i].isFinal) finalText += ` ${t}`
    else interim += ` ${t}`
  }
  finalText = finalText.trim()
  interim = interim.trim()

  if (wakeState.value === 'capturing') {
    if (interim) wakeHeard.value = interim
    if (finalText) {
      cancelWakeCapture()
      sendWakeCommand(finalText)
    }
    return
  }

  // Passive listening scans final results only — interim text flickers through
  // too many wrong guesses to trigger on.
  if (!finalText || wakeBusy()) return
  const m = wakeRe.exec(finalText)
  if (!m) return
  const command = finalText.slice(m.index + m[0].length).trim()
  wakeChime()
  if (command.length >= 2) {
    sendWakeCommand(command)
  } else {
    // Wake word alone — capture the next utterance as the command, or give up
    // after a quiet spell and drop back to passive listening.
    wakeState.value = 'capturing'
    wakeHeard.value = ''
    clearTimeout(wakeCaptureTimer)
    wakeCaptureTimer = setTimeout(cancelWakeCapture, 8000)
  }
}

function wakeRecStart() {
  if (!wakeRec) return
  try {
    wakeRec.start()
  } catch {
    // start() throws if this session is somehow still active — already running
    // is exactly the state the restart loop wants.
  }
}

function startWake() {
  if (!wakeSupported || wakeRec) return
  const rec = new SpeechRecognitionImpl()
  rec.continuous = true
  rec.interimResults = true
  rec.lang = 'en-US'
  rec.onresult = onWakeResult
  rec.onerror = (ev) => {
    if (ev.error === 'not-allowed' || ev.error === 'service-not-allowed') {
      // Permission is gone for good — flipping the toggle back on re-prompts.
      setWakeEnabled(false)
      toast('Microphone access was denied — wake word turned off.', false)
    }
    // Everything else ('no-speech', 'network', 'aborted') ends the session and
    // the onend restart loop brings it back.
  }
  rec.onend = () => {
    if (!wakeRec) return // stopped deliberately via stopWake()
    // Continuous sessions die on their own after silence or transient errors;
    // keep the loop alive for as long as the toggle is on.
    cancelWakeCapture()
    clearTimeout(wakeRestartTimer)
    wakeRestartTimer = setTimeout(wakeRecStart, 400)
  }
  wakeRec = rec
  wakeState.value = 'listening'
  wakeRecStart()
}

function stopWake() {
  clearTimeout(wakeRestartTimer)
  clearTimeout(wakeCaptureTimer)
  const rec = wakeRec
  wakeRec = null // onend sees null and stays down
  if (rec) {
    rec.onresult = rec.onerror = rec.onend = null
    try {
      rec.abort()
    } catch {
      // already stopped
    }
  }
  wakeState.value = 'off'
  wakeHeard.value = ''
}

function setWakeEnabled(on) {
  wakeEnabled.value = on
  try {
    localStorage.setItem('axon-wake-word', on ? '1' : '0')
  } catch {
    // storage full/unavailable — the toggle still works for this session
  }
  if (on) startWake()
  else stopWake()
}

function toggleWake() {
  // The click is a user gesture — unlock the chime's AudioContext now so the
  // wake beep is allowed to play later without one.
  if (!wakeEnabled.value) ensureChimeCtx()
  setWakeEnabled(!wakeEnabled.value)
}

// The recognizer and MediaRecorder cannot share the mic reliably (Android
// hands it to whoever asked last), so wake pauses while push-to-talk records
// or transcribes and resumes once the composer is idle again.
watch(recState, (s) => {
  if (!wakeEnabled.value) return
  if (s === 'idle') startWake()
  else stopWake()
})

function onVisibilityChange() {
  if (!wakeEnabled.value) return
  // Browsers kill background recognition anyway; stopping saves battery and
  // restarting on return makes the comeback deterministic.
  if (document.hidden) stopWake()
  else if (recState.value === 'idle') startWake()
}

function onWindowKeydown(e) {
  // Escape abandons a wake-word capture before anything is sent; checked
  // first so it cannot fall through to the run-stop branch.
  if (e.key === 'Escape' && wakeState.value === 'capturing') {
    e.preventDefault()
    cancelWakeCapture()
    return
  }
  // Escape discards an in-progress recording before it ever reaches the
  // transcriber; checked before the run-stop branch so recording wins.
  if (e.key === 'Escape' && recState.value === 'recording') {
    e.preventDefault()
    stopRecording(true)
    return
  }
  // Escape stops the current run while the chat page is visible.
  if (e.key === 'Escape' && disabled.value && inputEl.value && inputEl.value.offsetParent !== null) {
    e.preventDefault()
    stop()
    return
  }
  const active = document.activeElement
  const typingElsewhere =
    active &&
    active !== inputEl.value &&
    (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable)
  if (
    !disabled.value &&
    inputEl.value &&
    inputEl.value.offsetParent !== null && // chat page actually visible
    document.activeElement !== inputEl.value &&
    !typingElsewhere &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    e.key.length === 1
  ) {
    inputEl.value.focus()
  }
}

onMounted(async () => {
  connectWs(handleWsEvent)
  window.addEventListener('keydown', onWindowKeydown)
  document.addEventListener('visibilitychange', onVisibilityChange)
  // Wake word survives reloads: if it was on last visit, resume listening.
  // Chrome allows start() without a gesture once mic permission is granted;
  // if permission was revoked meanwhile, onerror turns the toggle back off.
  if (wakeEnabled.value) startWake()
  await loadConversations()

  // Every visit starts in a fresh conversation; past threads stay reachable
  // from the sidebar.
  newChat()

  nextTick(() => {
    focusComposer()
    adjustInputHeight()
    scrollBottom()
  })
})

onUnmounted(() => {
  window.removeEventListener('keydown', onWindowKeydown)
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopWake()
  stopRecording(true)
  clearInterval(recTimer)
  stopSpeaking()
})

watch(messages, () => scrollBottom(), { deep: true })
watch(wsStatus, (s) => {
  // If the socket drops mid-run the 'done' event never arrives; unlock the
  // input and mark the response as interrupted instead of spinning forever.
  if (s === 'disconnected' && disabled.value) {
    if (agentIdx >= 0) {
      const m = messages.value[agentIdx]
      m.thinking = false
      m.status = ''
      if (!m.text) m.text = 'Connection lost before a response arrived. Please try again.'
      else m.meta = 'interrupted — connection lost'
    }
    collapseTrace()
    resetRunTrackers()
    disabled.value = false
  }
})
watch(input, () => nextTick(adjustInputHeight))
watch(disabled, (newVal) => {
  if (!newVal) {
    setTimeout(() => {
      focusComposer()
      adjustInputHeight()
    }, 10)
  }
})
</script>

<template>
  <div class="chat-workspace">
    <aside class="conv-pane">
      <button
        class="conv-new"
        type="button"
        title="Start a new conversation"
        @click="newChat"
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <path
            d="M12 5v14M5 12h14"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
          />
        </svg>
        <span>New chat</span>
      </button>

      <SearchInput
        v-model="historySearch"
        :autofocus="false"
        class="conv-search"
        placeholder="Search chat history…"
      />

      <div class="conv-list">
        <p
          v-if="sidebarConversations.length === 0"
          class="conv-empty"
        >
          {{ historySearch.trim() ? 'No conversations match your search.' : 'No conversations yet.' }}
        </p>
        <div
          v-for="c in sidebarConversations"
          :key="c.id"
          class="conv-item"
          :class="{ active: c.id === currentSessionId }"
          @click="openConversation(c.id)"
        >
          <input
            v-if="renamingId === c.id"
            ref="renameEl"
            v-model="renameText"
            class="conv-rename"
            maxlength="60"
            @click.stop
            @dblclick.stop
            @mousedown.stop
            @keydown.enter.prevent="commitRename(c)"
            @keydown.esc.prevent="cancelRename"
            @blur="commitRename(c)"
          >
          <div
            v-else
            class="conv-item-text"
          >
            <span
              class="conv-title"
              title="Double-click to rename"
              @dblclick.stop="startRename(c)"
            >{{ c.title || 'New chat' }}</span>
            <span
              v-if="c.snippet"
              class="conv-snippet"
            >
              <template
                v-for="(seg, i) in highlightSegments(c.snippet)"
                :key="i"
              ><mark v-if="seg.mark">{{ seg.text }}</mark><template v-else>{{ seg.text }}</template></template>
            </span>
          </div>
          <button
            class="conv-del"
            type="button"
            title="Delete conversation"
            @click.stop="removeConversation(c.id)"
          >
            <svg
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
              aria-hidden="true"
            >
              <path
                d="M4 7h16M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
        </div>
      </div>
    </aside>

    <div class="chat-layout">
      <div
        ref="messagesEl"
        class="chat-messages"
      >
        <div
          v-if="messages.length === 0"
          class="chat-welcome"
        >
          <div class="chat-welcome-mark">
            <img
              src="/favicon.png"
              alt="Axon"
              class="logo-img chat-logo"
            >
          </div>
          <p class="welcome-desc">
            Ask for research, debugging, automation, or ops work. Axon can coordinate the details while you stay focused on decisions.
          </p>
          <div class="chat-starter-grid">
            <button
              v-for="prompt in starterPrompts"
              :key="prompt"
              type="button"
              class="chat-starter-btn"
              @click="useStarterPrompt(prompt)"
            >
              {{ prompt }}
            </button>
          </div>
        </div>

        <template
          v-for="(msg, idx) in messages"
          :key="idx"
        >
          <div
            v-if="msg.role === 'trace'"
            v-show="msg.trace.length > 0"
            class="tool-trace"
          >
            <button
              class="trace-toggle"
              type="button"
              @click="msg.collapsed = !msg.collapsed"
            >
              <span
                class="trace-chevron"
                :class="{ open: !msg.collapsed }"
              >▸</span>
              Reasoning · {{ msg.trace.length }} step{{ msg.trace.length === 1 ? '' : 's' }}
            </button>
            <div
              v-show="!msg.collapsed"
              class="trace-items"
            >
              <div
                v-for="(item, i) in msg.trace"
                :key="i"
                class="tool-trace-item"
              >
                <span :style="{ color: item.color }">{{ item.text }}</span>
              </div>
            </div>
          </div>

          <div
            v-else-if="msg.role === 'user'"
            class="chat-msg user"
          >
            <div class="chat-bubble">
              {{ msg.text }}
            </div>
          </div>

          <div
            v-else-if="msg.role === 'agent'"
            class="chat-msg agent"
          >
            <div class="chat-bubble">
              <span
                v-if="msg.thinking"
                class="thinking-indicator"
              >{{ msg.status || 'Thinking...' }}</span>
              <span
                class="chat-markdown"
                v-html="renderMarkdown(msg.text)"
              />
            </div>
            <div
              v-if="msg.meta || (canSpeak && msg.text && !msg.thinking)"
              class="chat-meta"
            >
              <button
                v-if="canSpeak && msg.text && !msg.thinking"
                class="msg-speak"
                :class="{ speaking: speakingIdx === idx }"
                type="button"
                :title="speakingIdx === idx ? 'Stop reading' : 'Read aloud'"
                @click="toggleSpeak(idx)"
              >
                <svg
                  v-if="speakingIdx !== idx"
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  xmlns="http://www.w3.org/2000/svg"
                  aria-hidden="true"
                >
                  <path
                    d="M11 5 6 9H3v6h3l5 4V5Z"
                    stroke="currentColor"
                    stroke-width="1.8"
                    stroke-linejoin="round"
                  />
                  <path
                    d="M15.5 8.5a5 5 0 0 1 0 7"
                    stroke="currentColor"
                    stroke-width="1.8"
                    stroke-linecap="round"
                  />
                  <path
                    d="M18.5 6a9 9 0 0 1 0 12"
                    stroke="currentColor"
                    stroke-width="1.8"
                    stroke-linecap="round"
                  />
                </svg>
                <svg
                  v-else
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  xmlns="http://www.w3.org/2000/svg"
                  aria-hidden="true"
                >
                  <rect
                    x="6"
                    y="6"
                    width="12"
                    height="12"
                    rx="2"
                    fill="currentColor"
                  />
                </svg>
              </button>
              <span v-if="msg.meta">{{ msg.meta }}</span>
            </div>
          </div>
        </template>
      </div>

      <div class="chat-input-area">
        <div class="chat-input-floating">
          <textarea
            ref="inputEl"
            v-model="input"
            class="chat-textarea"
            :disabled="disabled"
            placeholder="Message Axon..."
            rows="1"
            @keydown="onKeydown"
          />
          <button
            v-if="wakeSupported"
            class="btn-mic btn-wake"
            :class="{ 'is-listening': wakeState === 'listening', 'is-capturing': wakeState === 'capturing' }"
            type="button"
            :title="wakeEnabled ? 'Wake word is on — say “Hey Axon” (click to turn off)' : 'Turn on the “Hey Axon” wake word'"
            @click="toggleWake"
          >
            <svg
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
              aria-hidden="true"
            >
              <path
                d="M4 10v4"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
              <path
                d="M8 7v10"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
              <path
                d="M12 4v16"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
              <path
                d="M16 7v10"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
              <path
                d="M20 10v4"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
            </svg>
          </button>
          <button
            v-if="micSupported"
            class="btn-mic"
            :class="{ 'is-recording': recState === 'recording' }"
            type="button"
            :disabled="disabled || recState === 'transcribing'"
            :title="recState === 'recording' ? 'Stop and transcribe (Esc to cancel)' : 'Dictate a message'"
            @click="recState === 'recording' ? stopRecording() : startRecording()"
          >
            <span
              v-if="recState === 'transcribing'"
              class="mic-spinner"
              aria-hidden="true"
            />
            <svg
              v-else
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
              aria-hidden="true"
            >
              <rect
                x="9"
                y="3"
                width="6"
                height="11"
                rx="3"
                stroke="currentColor"
                stroke-width="2"
              />
              <path
                d="M5 11a7 7 0 0 0 14 0"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
              <path
                d="M12 18v3"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
            </svg>
          </button>
          <button
            class="btn-chat-send"
            :class="{ 'is-stop': disabled }"
            :disabled="!disabled && !input.trim()"
            :title="disabled ? 'Stop (Esc)' : 'Send (Enter)'"
            @click="disabled ? stop() : send()"
          >
            <svg
              v-if="!disabled"
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
            >
              <path
                d="M22 2L11 13"
                stroke="currentColor"
                stroke-width="2.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
              <path
                d="M22 2L15 22L11 13L2 9L22 2Z"
                fill="currentColor"
                opacity="0.4"
              />
              <path
                d="M22 2L15 22L11 13L2 9L22 2Z"
                stroke="currentColor"
                stroke-width="2.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <svg
              v-else
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              xmlns="http://www.w3.org/2000/svg"
              aria-hidden="true"
            >
              <rect
                x="6"
                y="6"
                width="12"
                height="12"
                rx="2.5"
                fill="currentColor"
              />
            </svg>
          </button>
        </div>
        <div class="chat-hints">
          <template v-if="recState === 'recording'">
            <span class="hint rec-hint"><span
              class="rec-dot"
              aria-hidden="true"
            />Recording {{ recClock }} — click the mic to transcribe, Esc to cancel</span>
          </template>
          <template v-else-if="recState === 'transcribing'">
            <span class="hint">Transcribing…</span>
          </template>
          <template v-else-if="wakeState === 'capturing'">
            <span class="hint rec-hint"><span
              class="rec-dot"
              aria-hidden="true"
            />Listening — say your command{{ wakeHeard ? `: “${wakeHeard}”` : '… (Esc to cancel)' }}</span>
          </template>
          <template v-else>
            <span class="hint">Enter to send</span>
            <span class="hint">Shift+Enter for a new line</span>
            <span
              v-if="wakeState === 'listening'"
              class="hint"
            >Say “Hey Axon” to talk</span>
          </template>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.trace-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0;
  border: none;
  background: transparent;
  color: inherit;
  font: inherit;
  font-size: 12px;
  opacity: 0.75;
  cursor: pointer;
}

.trace-toggle:hover {
  opacity: 1;
}

.trace-chevron {
  display: inline-block;
  transition: transform 0.15s ease;
}

.trace-chevron.open {
  transform: rotate(90deg);
}

.trace-items {
  margin-top: 6px;
}

.chat-workspace {
  display: flex;
  flex-direction: row;
  height: 100%;
  width: 100%;
  min-height: 0;
}

.chat-workspace > .chat-layout {
  flex: 1;
  min-width: 0;
}

.conv-pane {
  width: 250px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px 10px;
  border-right: 1px solid rgba(148, 163, 184, 0.18);
  min-height: 0;
}

.conv-new {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 10px 12px;
  border-radius: 10px;
  border: 1px solid rgba(148, 163, 184, 0.28);
  background: transparent;
  color: inherit;
  font-size: 0.9rem;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.15s ease, border-color 0.15s ease;
}

.conv-new:hover {
  background: rgba(148, 163, 184, 0.12);
  border-color: rgba(148, 163, 184, 0.45);
}

.conv-search {
  margin-bottom: 8px;
}

.conv-list {
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-height: 0;
}

.conv-empty {
  font-size: 0.82rem;
  opacity: 0.55;
  padding: 8px 6px;
}

.conv-item {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 9px 10px;
  border-radius: 9px;
  cursor: pointer;
  transition: background 0.12s ease;
}

.conv-item:hover {
  background: rgba(148, 163, 184, 0.1);
}

.conv-item.active {
  background: rgba(94, 234, 212, 0.14);
}

.conv-item-text {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.conv-title {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-size: 0.88rem;
}

.conv-snippet {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-size: 0.76rem;
  opacity: 0.65;
}

.conv-snippet mark {
  background: rgba(94, 234, 212, 0.35);
  color: inherit;
  border-radius: 2px;
  padding: 0 1px;
}

.conv-rename {
  flex: 1;
  min-width: 0;
  font-size: 0.88rem;
  font-family: inherit;
  color: inherit;
  background: rgba(15, 23, 42, 0.35);
  border: 1px solid rgba(94, 234, 212, 0.5);
  border-radius: 6px;
  padding: 3px 6px;
  outline: none;
}

.conv-del {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 4px;
  border: none;
  background: transparent;
  color: inherit;
  opacity: 0;
  border-radius: 6px;
  cursor: pointer;
  transition: opacity 0.12s ease, background 0.12s ease, color 0.12s ease;
}

.conv-item:hover .conv-del,
.conv-item.active .conv-del {
  opacity: 0.6;
}

.conv-del:hover {
  opacity: 1 !important;
  background: rgba(239, 68, 68, 0.15);
  color: #f87171;
}

/* ── Voice input ────────────────────────────────────────────────────────── */
.btn-mic {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  margin-left: 8px;
  padding: 0;
  flex-shrink: 0;
  border: 1px solid color-mix(in srgb, var(--text) 18%, transparent);
  border-radius: var(--r-md);
  background: transparent;
  color: inherit;
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease, background 0.15s ease;
}

.btn-mic:not(:disabled):hover {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
}

.btn-mic:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.btn-mic.is-recording {
  color: var(--red);
  background: var(--redDim);
  border-color: color-mix(in srgb, var(--red) 55%, transparent);
  animation: mic-pulse 1.6s ease-in-out infinite;
}

@keyframes mic-pulse {
  0%,
  100% {
    box-shadow: 0 0 0 0 color-mix(in srgb, var(--red) 35%, transparent);
  }
  50% {
    box-shadow: 0 0 0 6px transparent;
  }
}

.mic-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid color-mix(in srgb, var(--text) 25%, transparent);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: mic-spin 0.8s linear infinite;
}

@keyframes mic-spin {
  to {
    transform: rotate(360deg);
  }
}

.rec-hint {
  display: inline-flex;
  align-items: center;
  color: var(--red);
}

.rec-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  margin-right: 6px;
  border-radius: 50%;
  background: var(--red);
  animation: rec-blink 1s ease-in-out infinite;
}

@keyframes rec-blink {
  50% {
    opacity: 0.3;
  }
}

/* ── Wake word ──────────────────────────────────────────────────────────── */
/* Passive listening tints the button; capturing borrows the recording look so
   "the mic is hot" reads the same everywhere. On phones the hints row is
   hidden by the mobile layer, so these states are the only feedback. */
.btn-wake.is-listening {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
}

.btn-wake.is-capturing {
  color: var(--red);
  background: var(--redDim);
  border-color: color-mix(in srgb, var(--red) 55%, transparent);
  animation: mic-pulse 1.6s ease-in-out infinite;
}

/* ── Read aloud ─────────────────────────────────────────────────────────── */
.msg-speak {
  display: inline-flex;
  align-items: center;
  padding: 2px;
  margin-right: 6px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  opacity: 0;
  cursor: pointer;
  vertical-align: middle;
  transition: opacity 0.12s ease, color 0.12s ease;
}

.chat-msg.agent:hover .msg-speak,
.msg-speak.speaking {
  opacity: 0.65;
}

.msg-speak:hover {
  opacity: 1;
  color: var(--accent);
}

@media (max-width: 720px) {
  .conv-pane {
    width: 60px;
    padding: 10px 6px;
  }
  .conv-new span,
  .conv-title,
  .conv-empty {
    display: none;
  }
  .conv-item {
    justify-content: center;
  }
}
</style>
