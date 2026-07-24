<script setup>
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { subscribe, wsSend, wsStatus } from '../lib/ws.js'
import { get, put, del, postForm, postRaw } from '../lib/api.js'
import { toast, notifyBell } from '../lib/toast.js'
import { addNotification } from '../lib/notifications.js'
import { confirmDialog } from '../lib/confirm.js'
import { renderMarkdown } from '../lib/markdown.js'
import { createWakeWord, wakeWordSupported, FOLLOWUP_CAPTURE } from '../lib/wakeword.js'
import {
  prefetchPrompts,
  playPrompt,
  stopPrompt,
  randomWakeAck,
  WAKE_ACKS,
} from '../lib/voiceprompts.js'
import { buildTtsEnvelope } from '../lib/audioLevel.js'
import SearchInput from '../components/SearchInput.vue'
import VoiceOrb from '../components/VoiceOrb.vue'

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
// On phones (<768px) the history pane becomes an off-canvas drawer; this
// drives it. Desktop ignores it — the pane is always in flow there.
const historyOpen = ref(false)

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
  // noAnim: rehydrated history must not replay the fade-in-up entrance —
  // reopening a long thread otherwise animates every bubble at once.
  return m.role === 'assistant'
    ? { role: 'agent', text: m.content, thinking: false, meta: '', status: '', noAnim: true }
    : { role: 'user', text: m.content, noAnim: true }
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
  historyOpen.value = false
  nextTick(() => focusComposer())
}

async function openConversation(id) {
  historyOpen.value = false
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
  scrollBottom(true)
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
        // Voice reply: feed tokens to the streamed read-aloud so speech starts
        // with the first sentence, not after 'done'. Server TTS only (the
        // browser voice can't synthesize incrementally); audioSupported gates it.
        if (speakReplyOnDone && audioSupported) {
          if (!streamingSpeech) streamingSpeech = makeStreamingSpeech(agentIdx)
          streamingSpeech.append(ev.text)
        }
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
      // Server-wide broadcasts (empty run_id) belong to App.vue's app-wide
      // handler — handling them here too would double them in the bell.
      if (!ev.run_id) break
      if (currentRunId && ev.run_id !== currentRunId) break
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
      // The reply owns the speaker from here. A wake ack whose on-demand
      // synthesis is still in flight would otherwise land mid-reply and talk
      // over it, so invalidate it — whether or not this reply is about to be
      // read aloud (toggleSpeak's stopSpeaking only covers the speaking case).
      stopPrompt()
      if (speakReplyOnDone) {
        speakReplyOnDone = false
        if (agentIdx >= 0 && canSpeak) {
          const wantFollowup = wakeEnabled.value && wake?.running
          if (streamingSpeech && streamingSpeech.hasContent()) {
            // The reply was streamed sentence-by-sentence during generation;
            // just flush the tail and let it play out. followupEligible is set
            // BEFORE finish() because a short reply can drain synchronously and
            // fire the controller's natural-end callback (which reads it) now.
            followupEligible = wantFollowup
            streamingSpeech.finish()
          } else {
            // No streamed audio (server sent full_text only, or no Audio API):
            // speak the whole reply in one shot. Arm follow-up AFTER toggleSpeak,
            // whose synchronous stopSpeaking() prefix would clear the flag.
            abortStreamingSpeech()
            toggleSpeak(agentIdx)
            followupEligible = wantFollowup
          }
        }
      }
      collapseTrace()
      resetRunTrackers()
      disabled.value = false
      // Reconcile the sidebar: a brand-new thread now has a backend title, and
      // the active thread bubbles to the top by updated_at.
      loadConversations()
      flushPendingVoice()
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
      abortStreamingSpeech()
      stopPrompt()
      collapseTrace()
      resetRunTrackers()
      disabled.value = false
      endHandsFree()
      flushPendingVoice()
      break
  }
}

// instant: jump without the CSS smooth-scroll — opening an old conversation
// must not animate from the top of the whole transcript.
async function scrollBottom(instant = false) {
  await nextTick()
  if (messagesEl.value) {
    if (instant) messagesEl.value.scrollTo({ top: messagesEl.value.scrollHeight, behavior: 'instant' })
    else messagesEl.value.scrollTop = messagesEl.value.scrollHeight
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
  input.value = ''
  adjustInputHeight()
  await sendMessage(msg, false)
}

// The one path into a run: push the user bubble, open the trace + agent
// placeholders, and ship the task. Voice sends (push-to-talk and the wake
// word) call this directly with voice=true — spoken text never routes through
// the composer, and the reply is read aloud when the run completes.
async function sendMessage(msg, voice) {
  if (!msg || disabled.value) return
  // A new turn takes over the speaker: retire any reply still being read aloud
  // so its tail can't play over — or fire a follow-up during — the new run.
  abortStreamingSpeech()
  speakReplyOnDone = voice
  if (!currentSessionId.value) newChat()

  messages.value.push({ role: 'user', text: msg })
  disabled.value = true

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
  // `voice` tells the agent the reply will be read aloud, so it answers with a
  // short spoken summary instead of a raw dump (see the server SPOKEN REPLY hint).
  const sent = wsSend({ task: msg, session_id: currentSessionId.value, voice })
  if (!sent) {
    // Socket is down — undo the placeholders and give the message back
    // instead of dropping it silently and locking the input forever. A spoken
    // message has no composer to return to, so it's kept in the bell instead.
    messages.value.splice(voice ? traceIdx - 1 : traceIdx, voice ? 3 : 2)
    traceIdx = -1
    agentIdx = -1
    disabled.value = false
    endHandsFree()
    if (voice) notifyBell(`Voice message not sent — not connected: "${msg}"`, false)
    else input.value = msg
    toast('Not connected to the agent yet — retry once the status shows Connected.', false)
  }
}

// A transcript that landed while a run was streaming is queued; deliver it the
// moment the composer unlocks so voice input is never silently dropped.
function flushPendingVoice() {
  if (!pendingVoiceText || disabled.value) return
  const msg = pendingVoiceText
  pendingVoiceText = null
  sendMessage(msg, true)
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
  abortStreamingSpeech()
  stopPrompt()
  collapseTrace()
  resetRunTrackers()
  disabled.value = false
  endHandsFree()
  flushPendingVoice()
}

function onKeydown(e) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    send()
  }
}

// ── Voice input (mic → /api/audio/transcribe → send) ────────────────────────
// One button cycles idle → recording → transcribing → idle. The transcript
// sends straight into the conversation as its own message (speak-and-go, like
// the messaging gateways) and never routes through the composer — a typed
// draft survives a voice message untouched. If a run is already streaming when
// transcription lands, the text queues and sends the moment the run finishes.
const recState = ref('idle') // 'idle' | 'recording' | 'transcribing'
const recSeconds = ref(0)
let mediaRecorder = null
let recChunks = []
let recTimer = null
let recCancelled = false
let pendingVoiceText = null // transcript waiting out a streaming run
// A voice-initiated run has its reply read aloud on 'done'; typed sends never
// do. One run at a time (disabled gate), so a single flag is enough.
let speakReplyOnDone = false

// Browser echoCancellation is unreliable on the always-open wake mic, so the
// spoken ack ("Yes?") and the read-aloud reply can still bleed into the
// command capture and be transcribed — once sent, the agent would answer its
// own voice (e.g. a transcribed "yes" → a reply to "yes"). The Android app
// guards this with isSelfEcho; the web mirrors it as a transcript-level net:
// a transcript whose every word is among the ack phrases or the last spoken
// reply is dropped silently, never sent. Real commands ("what day is today")
// always pass — their words aren't in the reference set.
const SELF_ECHO_REF = new Set(
  [...WAKE_ACKS]
    .join(' ')
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(Boolean)
)
let lastSpokenText = '' // set when a reply starts playing; cleared on stop

function isSelfEcho(text) {
  const words = String(text || '')
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(Boolean)
  if (words.length === 0) return true
  if (words.length > 12) return false
  if (lastSpokenText) {
    const replyWords = new Set(lastSpokenText.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean))
    if (words.every((w) => SELF_ECHO_REF.has(w) || replyWords.has(w))) return true
  } else {
    if (words.every((w) => SELF_ECHO_REF.has(w))) return true
  }
  return false
}

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

async function startRecording(sharedStream = null) {
  if (recState.value !== 'idle' || disabled.value) return
  // While the wake word listener holds the mic, reuse its stream — a second
  // getUserMedia on the same device can steal the mic on Android.
  const shared = sharedStream || (wake?.running ? wake.stream : null)
  let stream = shared
  if (!stream) {
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    } catch {
      toast('Microphone access was denied — allow it for this site and try again.', false)
      return
    }
  }
  const mime = recorderMime()
  try {
    mediaRecorder = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream)
  } catch {
    if (!shared) stream.getTracks().forEach((t) => t.stop())
    toast('Audio recording is not supported in this browser.', false)
    return
  }
  recChunks = []
  recCancelled = false
  mediaRecorder.ondataavailable = (e) => {
    if (e.data && e.data.size > 0) recChunks.push(e.data)
  }
  mediaRecorder.onstop = () => {
    if (!shared) stream.getTracks().forEach((t) => t.stop()) // wake owns its stream
    wake?.cancelSilenceWatch()
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
  // A voice exchange is underway, so an ack is probably about to be needed:
  // warm the cache now (no-op for phrases already there) rather than paying the
  // synthesis round-trip mid-run, where it would fall back to the browser voice.
  prefetchPrompts()
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
      notifyBell(`Voice transcription failed: ${res.error}`, false)
      endHandsFree()
    } else if (!text) {
      toast('No speech detected in the recording.', false)
      endHandsFree()
    } else if (wakeEnabled.value && isSelfEcho(text)) {
      // The capture was the assistant's own voice bouncing back (ack phrase or
      // a fragment of the reply just read aloud) — drop it so it can't be sent
      // as a command and answered, looping the conversation.
      endHandsFree()
    } else if (disabled.value) {
      // A run is streaming — sending is blocked right now, so queue the
      // transcript; the done/error/stop handlers flush it as its own message.
      pendingVoiceText = pendingVoiceText ? `${pendingVoiceText} ${text}` : text
    } else {
      sendMessage(text, true)
    }
    // The self-echo reference only applies to the capture that just ended; once
    // we've applied the check, the spoken reply is stale for the next capture.
    lastSpokenText = ''
  } catch {
    notifyBell('Transcription failed — check the Voice Input settings.', false)
    endHandsFree()
  } finally {
    recState.value = 'idle'
    recSeconds.value = 0
  }
}

// ── Wake word ("Hey Axon", rustpotter WASM) ─────────────────────────────────
// On-device keyword spotting (see lib/wakeword.js) — no Web Speech API. While
// enabled, one mic stream stays open (steady OS indicator, audio never leaves
// the device). On detection: a spoken ack ("Yes?" — see lib/voiceprompts.js,
// chime only when nothing can speak), record the command through the normal
// push-to-talk pipeline (auto-stopped by the silence watcher), transcribe
// server-side, auto-send, and the reply is read aloud like any voice send.
// When that spoken reply finishes naturally, follow-up mode briefly reopens
// the mic so the next command needs no wake word (see startFollowupCapture).
const wakeSupported = micSupported && wakeWordSupported
const wakeEnabled = ref(wakeSupported && localStorage.getItem('axon-wake-word') === '1')
const wakeState = ref('off') // 'off' | 'starting' | 'listening'
let wake = null

// ── Hands-free overlay (JARVIS-style orb) ────────────────────────────────────
// While a "Hey Axon" exchange is actively in progress — recording, waiting on
// the agent, or reading the reply aloud — the chat log and composer are
// covered by a full-panel animated orb instead: the point of hands-free is
// not staring at text. The exchange still writes into `messages` exactly as
// before, so scrolling back once the overlay closes shows the normal
// transcript. Manual typing and push-to-talk (mic button) never set this —
// only the two wake-triggered recording entry points do.
const handsFreeActive = ref(false)
const handsFreePhase = ref('listening') // 'listening' | 'thinking' | 'speaking'

// The mic ('listening') feeds the orb the wake stream's own AnalyserNode
// (wakeword.js) — a passive read that never touches playback. The 'speaking'
// phase is handled separately by speakSample() below (a decoded envelope, not
// an analyser), so orbAnalyser stays mic-only.
const orbAnalyser = computed(() =>
  handsFreePhase.value === 'listening' ? wake?.analyser || null : null
)

const handsFreeStatusText = computed(
  () =>
    ({ listening: 'Listening…', thinking: 'Thinking…', speaking: 'Axon is speaking…' })[handsFreePhase.value] || ''
)

function endHandsFree() {
  handsFreeActive.value = false
}

// The overlay's close button: bail out of hands-free back to the normal chat
// view without turning "Hey Axon" off — cancels whatever turn is in flight
// (recording, the agent run, or the read-aloud) the same way the equivalent
// manual control would.
function dismissHandsFree() {
  if (recState.value === 'recording') stopRecording(true)
  else if (disabled.value) stop()
  else stopSpeaking()
  endHandsFree()
}

async function onWakeDetected() {
  // A streaming run, an active recording/transcription, or read-aloud playback
  // disqualifies a trigger — the speaking guard keeps the assistant from
  // waking itself off its own voice coming out of the speakers.
  if (disabled.value || recState.value !== 'idle' || speakingIdx.value >= 0) return
  handsFreeActive.value = true
  handsFreePhase.value = 'listening'
  // Answer first, then open the mic — the same order the Android client uses
  // (play the ack blocking, then capture). Recording *through* the ack meant
  // the assistant's own "I'm listening" bled into the capture and rode along on
  // the command ("I'm listening turn on the lights"): echo cancellation on the
  // always-open wake mic doesn't reliably cancel a full spoken phrase, and the
  // self-echo net only drops a capture that is *nothing but* ack words, so an
  // ack glued to a real command slips through. The acks are short, so the beat
  // after them is where the user naturally starts talking anyway.
  const spoke = await playPrompt(randomWakeAck())
  if (!spoke) wake.chime()
  // The ack took ~0.5-1s; re-check the guards in case a run started, the tab
  // was hidden (which tears down the mic), or the user touched a control while
  // it played.
  if (disabled.value || recState.value !== 'idle' || speakingIdx.value >= 0 || !wake?.running) {
    endHandsFree()
    return
  }
  await startRecording(wake.stream)
  // Cancel the capture when the window closed without anyone actually speaking
  // (same contract the follow-up window uses). Uploading a silent clip is how
  // the transcriber ends up inventing a stock phrase — "Thank you." — and
  // sending it as if it were a command.
  if (recState.value === 'recording') {
    wake.watchSilence((hadSpeech) => {
      stopRecording(!hadSpeech)
      if (!hadSpeech) endHandsFree()
    })
  } else {
    endHandsFree()
  }
}

async function startWake() {
  if (!wakeSupported || wakeState.value !== 'off') return
  if (!wake) {
    wake = createWakeWord({
      onDetection: onWakeDetected,
      onState: (s) => {
        wakeState.value = s
      },
    })
  }
  wakeState.value = 'starting'
  try {
    await wake.start()
    // Warm the spoken-ack cache so "Yes?" plays the instant a wake is heard
    // (fire-and-forget; misses fall back to the browser voice, then the chime).
    prefetchPrompts()
  } catch (err) {
    wakeState.value = 'off'
    wakeEnabled.value = false
    endHandsFree()
    try {
      localStorage.setItem('axon-wake-word', '0')
    } catch {
      // storage unavailable — session-only state is fine
    }
    const denied = err?.name === 'NotAllowedError'
    toast(
      denied
        ? 'Microphone access was denied — wake word turned off.'
        : 'The wake word engine failed to load — see the browser console.',
      false
    )
    if (!denied) console.error('wake word start failed:', err)
  }
}

function setWakeEnabled(on) {
  wakeEnabled.value = on
  try {
    localStorage.setItem('axon-wake-word', on ? '1' : '0')
  } catch {
    // storage unavailable — the toggle still works for this session
  }
  if (on) {
    startWake()
  } else {
    wake?.stop()
    endHandsFree()
  }
}

function toggleWake() {
  setWakeEnabled(!wakeEnabled.value)
}

// ── Follow-up mode ───────────────────────────────────────────────────────────
// After a wake-triggered reply finishes reading aloud, a soft chime plays and
// the mic reopens so the next command needs no "Hey Axon". Two guards keep
// bystanders out of the conversation: FOLLOWUP_CAPTURE raises the speech bar
// to ~2x normal (a voice close to the mic passes, people talking across the
// room don't) and allows ~5s to start answering; and a window that heard no
// qualifying speech is cancelled outright — nothing is transcribed, nothing is
// sent. The flag is armed only for auto-spoken voice replies (never manual
// read-aloud clicks) and cleared by stopSpeaking(), so a user stop also
// declines the follow-up.
let followupEligible = false

function followupClear() {
  return (
    wakeEnabled.value &&
    wake?.running &&
    !disabled.value &&
    recState.value === 'idle' &&
    speakingIdx.value < 0 &&
    !document.hidden
  )
}

// Long enough to cover the ~0.2s soft note plus its output-path tail.
const CHIME_SETTLE_MS = 300

function startFollowupCapture() {
  // Small gap after playback so the speaker tail can't register as speech.
  setTimeout(async () => {
    if (!followupClear()) return
    // The soft chime alone opens the window — no spoken prompt. The mic waits
    // for it to finish rather than opening behind it: it plays out of the same
    // speaker the mic is listening to, and hearing our own chime would count as
    // the speech onset this window is watching for, so an empty follow-up would
    // capture and transcribe itself instead of quietly cancelling.
    wake.chime(true)
    await new Promise((r) => setTimeout(r, CHIME_SETTLE_MS))
    if (!followupClear()) return // state may have shifted while the chime played
    handsFreeActive.value = true
    handsFreePhase.value = 'listening'
    startRecording(wake.stream)
    wake.watchSilence((hadSpeech) => {
      stopRecording(!hadSpeech)
      if (!hadSpeech) endHandsFree()
    }, FOLLOWUP_CAPTURE)
  }, 250)
}

function onVisibilityChange() {
  if (!wakeEnabled.value) return
  if (document.hidden) {
    // Release the mic in the background: the OS indicator turns off and the
    // battery is spared; listening resumes when the tab returns.
    if (recState.value === 'recording' && wake?.running) stopRecording(true)
    wake?.stop()
    endHandsFree()
  } else if (wakeState.value === 'off') {
    startWake()
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
let synthUtterance = null // pinned: Chrome goes silent if the utterance is GC'd
let ttsFailureToasted = false // explain a dead tts.* config once, not per click
let streamingSpeech = null // active per-sentence streamed read-aloud (voice replies)

// The reply audio the orb reacts to during 'speaking': the <audio> currently
// making sound and its decoded RMS envelope. speakSample() (passed to VoiceOrb)
// reads the envelope at the element's live currentTime, so the orb tracks what
// is audible. Nothing here touches the playback path — the envelope is decoded
// separately (lib/audioLevel.js) — so it can never mute the reply.
let speakEl = null
let speakEnv = null

function speakSample() {
  if (!speakEnv || !speakEl) return null
  return speakEnv.level(speakEl.currentTime)
}

function clearSpeakEnvelope() {
  speakEl = null
  speakEnv = null
}

// Attach the reactive envelope for a reply element: point the orb at it now,
// then decode in the background and swap the envelope in once ready (so the
// element never waits on the decode). No-op outside hands-free.
function attachSpeakEnvelope(el, blob) {
  if (!handsFreeActive.value) return
  speakEl = el
  speakEnv = null
  buildTtsEnvelope(blob).then((env) => {
    if (speakEl === el) speakEnv = env // still the current element
  })
}

// Hands-free orb phase follows the two state machines it's already wired
// into rather than being set from every call site by hand: recState covers
// listening/thinking, speakingIdx covers speaking. Both watchers are no-ops
// unless a wake-triggered exchange is actually active.
watch(recState, (state) => {
  if (!handsFreeActive.value) return
  if (state === 'recording') handsFreePhase.value = 'listening'
  else if (state === 'transcribing') handsFreePhase.value = 'thinking'
})
watch(speakingIdx, (idx) => {
  if (handsFreeActive.value && idx >= 0) handsFreePhase.value = 'speaking'
})
// Leaving 'speaking' (or the orb going away) retires the reply envelope so a
// later phase can't sample a stale, finished element.
watch(handsFreePhase, (phase) => {
  if (phase !== 'speaking') clearSpeakEnvelope()
})

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
  followupEligible = false
  stopPrompt()
  abortStreamingSpeech()
  speakSeq += 1
  if (speakAbort) {
    speakAbort.abort()
    speakAbort = null
  }
  releaseAudio()
  clearSpeakEnvelope()
  if (ttsSupported) window.speechSynthesis.cancel()
  synthUtterance = null
  speakingIdx.value = -1
}

// Today's zero-config path, now the fallback: the browser's built-in voice.
// Chrome needs three workarounds to actually make sound: speak() issued in the
// same tick as cancel() is silently dropped (hence the delay), an utterance
// with no live reference can be GC'd mid-sentence, and the queue sometimes
// comes back from cancel() stuck in the paused state.
function speakWithSynthesis(idx, text) {
  // Browser voice exposes no decodable audio — retire any server-TTS envelope
  // so the orb uses its synthetic talking pulse instead of a stale element.
  clearSpeakEnvelope()
  if (!ttsSupported) {
    if (speakingIdx.value === idx) speakingIdx.value = -1
    return
  }
  const synth = window.speechSynthesis
  synth.cancel()
  const u = new SpeechSynthesisUtterance(text)
  synthUtterance = u
  // Split handlers for the same reason as the audio element: only a natural
  // end may open the follow-up window, never an error or a user stop.
  u.onend = () => {
    if (synthUtterance === u) synthUtterance = null
    if (speakingIdx.value === idx) {
      speakingIdx.value = -1
      const followup = followupEligible
      followupEligible = false
      if (followup) {
        // Neutral "thinking" until the follow-up mic actually reopens (a
        // ~550ms gap of chime + settle) — a bare fake "speaking" pulse with
        // nothing left to say would read as stuck, not alive.
        handsFreePhase.value = 'thinking'
        startFollowupCapture()
      } else {
        endHandsFree()
      }
    }
  }
  u.onerror = () => {
    if (synthUtterance === u) synthUtterance = null
    followupEligible = false
    if (speakingIdx.value === idx) speakingIdx.value = -1
    endHandsFree()
  }
  const seq = speakSeq
  setTimeout(() => {
    if (seq !== speakSeq) return
    synth.speak(u)
    synth.resume()
  }, 150)
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
  // Remember the reply we're about to speak so a capture that echoes it back
  // can be dropped by isSelfEcho() instead of answered.
  lastSpokenText = text

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
        // Drive the hands-free orb from this reply's decoded envelope.
        attachSpeakEnvelope(audioEl, blob)
        // Natural end and failure diverge: only a played-to-the-end reply may
        // open the follow-up window (read the flag before stopSpeaking clears it).
        audioEl.onended = () => {
          if (speakingIdx.value !== idx) return
          const followup = followupEligible
          stopSpeaking()
          if (followup) {
            handsFreePhase.value = 'thinking'
            startFollowupCapture()
          } else {
            endHandsFree()
          }
        }
        audioEl.onerror = () => {
          if (speakingIdx.value === idx) stopSpeaking()
          endHandsFree()
        }
        await audioEl.play()
        return
      }
      // Non-audio answer — say why once, or a dead tts.* config is
      // indistinguishable from the fallback voice.
      let detail = ''
      try {
        detail = (await res.json())?.error || ''
      } catch {
        // proxy HTML error page or empty body — status alone will have to do
      }
      console.warn(`Server TTS unavailable (${res.status}): ${detail || 'no detail'}`)
      const ttsMsg = detail
        ? `Server TTS error: ${detail}`
        : `Server TTS unavailable (HTTP ${res.status}) — using the browser voice instead.`
      // Every failure lands in the bell log; the toast still fires only once
      // per session so a dead tts.* config doesn't nag on every click.
      addNotification(ttsMsg, false)
      if (!ttsFailureToasted) {
        ttsFailureToasted = true
        toast(ttsMsg, false)
      }
    } catch {
      // Aborted, network failure, or blocked autoplay — clean up whatever the
      // attempt allocated; the seq guard below decides whether to fall back.
      // A user stop (seq bumped) is not an error; anything else is bell-only —
      // the browser-voice fallback keeps the moment itself quiet.
      if (seq === speakSeq) {
        releaseAudio()
        addNotification('Server TTS request failed (network or playback) — using the browser voice instead.', false)
      }
    }
    if (seq !== speakSeq) return // user hit stop during the attempt
  }
  speakWithSynthesis(idx, text)
}

// ── Streaming read-aloud (spoken while the reply is still being written) ─────
// A voice reply is spoken as it generates, not after: reply tokens are split
// into sentences and each is synthesized (server TTS) and played back-to-back,
// so the first sentence is heard while the rest is still streaming — the same
// design as the Android client's StreamingTts. Time-to-first-audio drops from
// "synthesize the whole reply" to "synthesize one sentence". Server TTS only;
// if it is unavailable for the whole reply we defer to the browser voice when
// generation finishes (instant, no per-sentence benefit lost), never silent.

// Index just past the first sentence boundary in [s], or -1 if none yet — a
// direct port of the Android StreamingTts splitter so both clients chunk the
// same way (`. ` `! ` `? ` or a newline, trailing quotes/brackets included).
function nextSpeechBoundary(s) {
  const n = s.length
  for (let i = 0; i < n; i++) {
    const c = s[i]
    if (c === '\n' || c === '\r') return i + 1
    if (c === '.' || c === '!' || c === '?') {
      let j = i + 1
      while (j < n && !/\s/.test(s[j])) {
        const cj = s[j]
        if (cj !== '"' && cj !== "'" && cj !== ']' && cj !== ')' && cj !== '”' && cj !== '’') break
        j++
      }
      if (j < n && /\s/.test(s[j])) return j + 1
      if (j >= n) return -1 // boundary at the tail — flushed by finish()
    }
  }
  return -1
}

class StreamingSpeech {
  constructor(idx, onNaturalEnd, onFallback) {
    this.idx = idx
    this.onNaturalEnd = onNaturalEnd // fired once playback drains naturally
    this.onFallback = onFallback // (fullText) => browser-voice the whole reply
    this.buf = '' // undrained text still being split into sentences
    this.full = '' // every token, for the fallback voice
    this.pending = [] // complete sentences awaiting synthesis
    this.playQueue = [] // synthesized blob URLs awaiting playback
    this.synthing = false
    this.playing = false
    this.finished = false // finish() called (no more tokens)
    this.aborted = false
    this.ended = false
    this.fellBack = false
    this.serverDown = false // server TTS failed → collect text, browser voice later
    this.anyServer = false // at least one sentence synthesized by the server
    this.curAudio = null
    this.curUrl = null
    this.fetchAbort = null
  }

  hasContent() {
    return this.full.trim().length > 0
  }

  append(token) {
    if (this.aborted || this.finished || !token) return
    this.full += token
    if (this.serverDown) return // keep only the full text for the browser voice
    this.buf += token
    let i
    while ((i = nextSpeechBoundary(this.buf)) >= 0) {
      const sentence = this.buf.slice(0, i).trim()
      this.buf = this.buf.slice(i)
      if (sentence) this.pending.push(sentence)
    }
    this.pumpSynth()
  }

  finish() {
    if (this.aborted || this.finished) return
    this.finished = true
    if (this.serverDown) {
      this.doFallback()
      return
    }
    const tail = this.buf.trim()
    this.buf = ''
    if (tail) this.pending.push(tail)
    this.pumpSynth()
    this.maybeFinish()
  }

  abort() {
    if (this.aborted) return
    this.aborted = true
    this.pending = []
    if (this.fetchAbort) {
      try {
        this.fetchAbort.abort()
      } catch {
        /* already settled */
      }
      this.fetchAbort = null
    }
    this.stopCurrentAudio()
    for (const item of this.playQueue) URL.revokeObjectURL(item.url)
    this.playQueue = []
  }

  stopCurrentAudio() {
    if (this.curAudio) {
      this.curAudio.onended = null
      this.curAudio.onerror = null
      try {
        this.curAudio.pause()
      } catch {
        /* not started */
      }
      this.curAudio = null
    }
    if (this.curUrl) {
      URL.revokeObjectURL(this.curUrl)
      this.curUrl = null
    }
  }

  async pumpSynth() {
    if (this.synthing || this.aborted || this.serverDown) return
    this.synthing = true
    while (this.pending.length && !this.aborted && !this.serverDown) {
      const sentence = this.pending.shift()
      let ok = false
      try {
        this.fetchAbort = new AbortController()
        const res = await postRaw('/audio/speech', { text: sentence }, this.fetchAbort.signal)
        this.fetchAbort = null
        if (this.aborted) return
        const type = res.headers.get('content-type') || ''
        if (res.ok && type.startsWith('audio/')) {
          const blob = await res.blob()
          if (this.aborted) return
          this.anyServer = true
          // Keep the blob so playback can decode its orb envelope (see
          // attachSpeakEnvelope); the URL is what the <audio> element plays.
          this.playQueue.push({ url: URL.createObjectURL(blob), blob })
          this.startPlaybackIfIdle()
          ok = true
        }
      } catch {
        this.fetchAbort = null
        if (this.aborted) return
      }
      // First failure with no server audio yet: stop trying and speak the whole
      // reply with the browser voice once it finishes (matches the whole-blob
      // path and the Android client). A failure after some audio already played
      // just drops that one sentence and keeps streaming.
      if (!ok && !this.anyServer) {
        this.serverDown = true
        this.pending = []
        break
      }
    }
    this.synthing = false
    if (this.serverDown && this.finished) {
      this.doFallback()
      return
    }
    this.maybeFinish()
  }

  startPlaybackIfIdle() {
    if (!this.playing && !this.aborted) this.playNext()
  }

  playNext() {
    if (this.aborted) return
    const item = this.playQueue.shift()
    if (item === undefined) {
      this.playing = false
      this.maybeFinish()
      return
    }
    const { url, blob } = item
    this.playing = true
    const el = new Audio(url)
    this.curAudio = el
    this.curUrl = url
    attachSpeakEnvelope(el, blob) // drive the hands-free orb from this sentence
    if (speakingIdx.value !== this.idx) speakingIdx.value = this.idx
    let advanced = false
    const advance = () => {
      if (advanced) return
      advanced = true
      if (this.curUrl === url) {
        URL.revokeObjectURL(url)
        this.curUrl = null
      }
      if (this.curAudio === el) this.curAudio = null
      this.playNext()
    }
    el.onended = advance
    el.onerror = advance
    // Autoplay is permitted here — playback only starts for a reply the user
    // asked for by voice — but a rejected/decoded-badly chunk is just skipped.
    el.play().catch(() => advance())
  }

  maybeFinish() {
    if (this.aborted || this.ended || this.fellBack || !this.finished) return
    if (this.synthing || this.pending.length || this.playing || this.playQueue.length) return
    if (this.anyServer) {
      this.ended = true
      this.aborted = true
      this.onNaturalEnd()
    } else {
      this.doFallback()
    }
  }

  doFallback() {
    if (this.fellBack) return
    this.fellBack = true
    this.aborted = true
    this.stopCurrentAudio()
    for (const item of this.playQueue) URL.revokeObjectURL(item.url)
    this.playQueue = []
    this.onFallback(this.full)
  }
}

// Build a controller bound to the [idx] agent bubble, wiring its two terminal
// callbacks into the same follow-up / self-echo bookkeeping toggleSpeak uses.
function makeStreamingSpeech(idx) {
  return new StreamingSpeech(
    idx,
    () => {
      // Natural end: the whole reply played out via server TTS.
      const followup = followupEligible
      followupEligible = false
      lastSpokenText = plainTextForSpeech(messages.value[idx]?.text)
      speakingIdx.value = -1
      streamingSpeech = null
      if (followup) {
        handsFreePhase.value = 'thinking'
        startFollowupCapture()
      } else {
        endHandsFree()
      }
    },
    (whole) => {
      // Server TTS never worked for this reply — speak it all with the browser
      // voice (speakWithSynthesis fires the follow-up from its own onend).
      streamingSpeech = null
      const text = plainTextForSpeech(whole)
      if (!text) {
        speakingIdx.value = -1
        followupEligible = false
        endHandsFree()
        return
      }
      lastSpokenText = text
      speakingIdx.value = idx
      speakWithSynthesis(idx, text)
    },
  )
}

// Tear down any active streamed reply without firing its terminal callbacks
// (a new send, a stop, an error, or a manual stopSpeaking took over).
function abortStreamingSpeech() {
  if (!streamingSpeech) return
  const idx = streamingSpeech.idx
  streamingSpeech.abort()
  streamingSpeech = null
  if (speakingIdx.value === idx) speakingIdx.value = -1
}

function onWindowKeydown(e) {
  // Escape dismisses the mobile history drawer before anything else.
  if (e.key === 'Escape' && historyOpen.value) {
    e.preventDefault()
    historyOpen.value = false
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

// App.vue owns the socket now (the bell needs it on every page); this page just
// adds its own handler for run-scoped events.
let unsubscribeWs = null

onMounted(async () => {
  unsubscribeWs = subscribe(handleWsEvent)
  window.addEventListener('keydown', onWindowKeydown)
  document.addEventListener('visibilitychange', onVisibilityChange)
  // Wake word survives reloads: getUserMedia without a gesture is allowed once
  // mic permission is granted; if it was revoked, startWake's catch turns the
  // toggle back off.
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
  unsubscribeWs?.()
  window.removeEventListener('keydown', onWindowKeydown)
  document.removeEventListener('visibilitychange', onVisibilityChange)
  stopRecording(true)
  wake?.stop()
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
    <aside
      class="conv-pane"
      :class="{ open: historyOpen }"
    >
      <div class="conv-drawer-head">
        <span>Conversations</span>
        <button
          class="conv-drawer-close"
          type="button"
          title="Close history"
          @click="historyOpen = false"
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
              d="M18 6 6 18M6 6l12 12"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
            />
          </svg>
        </button>
      </div>
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
            class="conv-edit"
            type="button"
            title="Rename conversation"
            @click.stop="startRename(c)"
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
                d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7.5 18.5 3 20l1.5-4.5Z"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
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

    <div
      v-if="historyOpen"
      class="conv-overlay"
      @click="historyOpen = false"
    />

    <div class="chat-layout">
      <div class="chat-mobile-bar">
        <button
          class="chat-mobile-btn"
          type="button"
          @click="historyOpen = true"
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
              d="M3 12a9 9 0 1 0 2.6-6.4L3 8"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
            <path
              d="M3 3v5h5"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
            <path
              d="M12 7.5V12l3 2"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
            />
          </svg>
          <span>History</span>
        </button>
        <button
          class="chat-mobile-btn chat-mobile-new"
          type="button"
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
      </div>

      <div
        v-if="handsFreeActive"
        class="handsfree-overlay"
      >
        <button
          class="handsfree-close"
          type="button"
          title="Close (keeps “Hey Axon” listening)"
          @click="dismissHandsFree"
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
              d="M18 6 6 18M6 6l12 12"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
            />
          </svg>
        </button>
        <VoiceOrb
          :phase="handsFreePhase"
          :analyser="orbAnalyser"
          :speak-sample="speakSample"
        />
        <div class="handsfree-status">
          {{ handsFreeStatusText }}
        </div>
      </div>

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
            :class="{ 'no-anim': msg.noAnim }"
          >
            <div class="chat-bubble">
              {{ msg.text }}
            </div>
          </div>

          <div
            v-else-if="msg.role === 'agent'"
            class="chat-msg agent"
            :class="{ 'no-anim': msg.noAnim }"
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
        <div
          v-if="recState === 'recording'"
          class="chat-voice-status rec"
        >
          <span
            class="rec-dot"
            aria-hidden="true"
          />
          <span>Recording {{ recClock }}</span>
          <button
            class="voice-cancel"
            type="button"
            @click="stopRecording(true)"
          >
            Cancel
          </button>
        </div>
        <div
          v-else-if="recState === 'transcribing'"
          class="chat-voice-status"
        >
          <span
            class="mic-spinner"
            aria-hidden="true"
          />
          <span>Transcribing…</span>
        </div>
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
            :class="{ 'is-listening': wakeState === 'listening' }"
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
        <div
          v-if="recState === 'idle'"
          class="chat-hints"
        >
          <span class="hint">Enter to send</span>
          <span class="hint">Shift+Enter for a new line</span>
          <span
            v-if="wakeState === 'listening'"
            class="hint"
          >Say “Hey Axon” to talk</span>
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

.conv-del,
.conv-edit {
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
.conv-item.active .conv-del,
.conv-item:hover .conv-edit,
.conv-item.active .conv-edit {
  opacity: 0.6;
}

.conv-del:hover {
  opacity: 1 !important;
  background: rgba(239, 68, 68, 0.15);
  color: #f87171;
}

.conv-edit:hover {
  opacity: 1 !important;
  background: color-mix(in srgb, var(--accent) 15%, transparent);
  color: var(--accent);
}

/* Rehydrated messages skip the entrance animation (flag set in rowToMessage). */
.chat-msg.no-anim {
  animation: none;
}

/* ── Voice status strip ─────────────────────────────────────────────────── */
/* Sits above the composer on every screen size. It replaced the hint-row
   text so recording / transcribing feedback survives on
   phones (where .chat-hints is hidden), and its Cancel button is the touch
   equivalent of Esc. */
.chat-voice-status {
  display: flex;
  align-items: center;
  gap: 8px;
  max-width: 100%;
  min-height: 32px;
  margin-bottom: 8px;
  padding: 4px 6px 4px 14px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--bg-card);
  font-size: 0.8rem;
  color: var(--muted);
}

.chat-voice-status.rec {
  color: var(--red);
  border-color: color-mix(in srgb, var(--red) 45%, transparent);
}

.chat-voice-status .rec-dot {
  margin-right: 0;
}

.voice-heard {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.voice-cancel {
  flex-shrink: 0;
  margin-left: 4px;
  min-height: 26px;
  padding: 0 12px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: transparent;
  color: var(--text);
  font: inherit;
  font-size: 0.76rem;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.12s ease;
}

.voice-cancel:hover {
  background: color-mix(in srgb, var(--text) 8%, transparent);
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
/* Passive listening tints the toggle; while a wake-triggered capture runs,
   the shared mic-button recording styles carry the "mic is hot" look. */
.btn-wake.is-listening {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
}

/* ── Hands-free overlay ─────────────────────────────────────────────────── */
/* Covers the log + composer with the animated orb (VoiceOrb.vue) for the
   duration of an active "Hey Axon" exchange — see handsFreeActive in the
   script. .chat-layout carries position: relative for this (style.css). */
.handsfree-overlay {
  position: absolute;
  inset: 0;
  z-index: 20;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 20px;
  background: color-mix(in srgb, var(--bg) 92%, transparent);
  backdrop-filter: blur(6px);
}

.handsfree-close {
  position: absolute;
  top: 16px;
  right: 16px;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border: 1px solid color-mix(in srgb, var(--text) 18%, transparent);
  border-radius: var(--r-md);
  background: transparent;
  color: var(--text);
  cursor: pointer;
  transition: color 0.15s ease, border-color 0.15s ease;
}

.handsfree-close:hover {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 45%, transparent);
}

.handsfree-status {
  font-family: var(--font-mono);
  font-size: 0.85rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: color-mix(in srgb, var(--text) 72%, transparent);
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

/* ── Phone layer (shares the shell's 768px breakpoint) ──────────────────── */
/* Hidden outside the phone breakpoint. */
.chat-mobile-bar,
.conv-drawer-head,
.conv-overlay {
  display: none;
}

@media (max-width: 767px) {
  /* Slim header inside the chat column: History opens the drawer, plus a
     reachable New chat (the drawer's own button is off-canvas). */
  .chat-mobile-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
  }

  .chat-mobile-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-height: 38px;
    padding: 0 13px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg-card);
    color: var(--text);
    font: inherit;
    font-size: 0.82rem;
    font-weight: 600;
    cursor: pointer;
  }

  .chat-mobile-btn:active {
    background: color-mix(in srgb, var(--text) 8%, transparent);
  }

  .chat-mobile-new {
    margin-left: auto;
  }

  /* The history pane becomes an off-canvas drawer. Relies on the mobile
     layer disabling the page-enter animation (<768px): its fill-mode keeps a
     transform on .page.active that would otherwise become this fixed
     element's containing block. */
  .conv-pane {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    width: min(320px, 86vw);
    z-index: 960; /* above the tab bar (890), below the shell drawer (1000) */
    background: var(--surface);
    border-right: 1px solid var(--border);
    padding: calc(12px + env(safe-area-inset-top)) 10px calc(12px + env(safe-area-inset-bottom)) max(10px, env(safe-area-inset-left));
    transform: translateX(-100%);
    visibility: hidden;
    transition: transform 0.26s cubic-bezier(0.4, 0, 0.2, 1), visibility 0s linear 0.26s;
  }

  .conv-pane.open {
    transform: none;
    visibility: visible;
    transition: transform 0.26s cubic-bezier(0.4, 0, 0.2, 1);
  }

  .conv-overlay {
    display: block;
    position: fixed;
    inset: 0;
    z-index: 950;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(3px);
    -webkit-backdrop-filter: blur(3px);
  }

  .conv-drawer-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 0 0 6px;
    font-size: 0.9rem;
    font-weight: 700;
  }

  .conv-drawer-close {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    border: none;
    border-radius: var(--r-md);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  .conv-drawer-close:active {
    background: color-mix(in srgb, var(--text) 8%, transparent);
  }

  /* Thumb-sized rows and composer buttons (44px minimum). */
  .conv-item {
    min-height: 44px;
  }

  .btn-mic {
    width: 44px;
    height: 44px;
    margin-left: 6px;
  }
}

/* Touch has no hover: the hover-revealed actions must stay visible, or
   delete / rename / read-aloud simply don't exist on phones. */
@media (hover: none) {
  .conv-item .conv-del,
  .conv-item .conv-edit {
    opacity: 0.45;
  }

  .chat-msg.agent .msg-speak {
    opacity: 0.55;
  }
}
</style>
