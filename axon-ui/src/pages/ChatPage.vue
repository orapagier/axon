<script setup>
import { ref, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { connectWs, wsSend, wsStatus } from '../lib/ws.js'
import { get, put, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { renderMarkdown } from '../lib/markdown.js'

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
const LS_KEY = 'axon.chat.session'

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
  return m.role === 'assistant'
    ? { role: 'agent', text: m.content, thinking: false, meta: '', status: '' }
    : { role: 'user', text: m.content }
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

function newChat() {
  currentSessionId.value = uuid()
  localStorage.setItem(LS_KEY, currentSessionId.value)
  messages.value = []
  resetRunTrackers()
  disabled.value = false
  nextTick(() => inputEl.value?.focus())
}

async function openConversation(id) {
  if (id === currentSessionId.value || disabled.value) return
  currentSessionId.value = id
  localStorage.setItem(LS_KEY, id)
  resetRunTrackers()
  try {
    const res = await get(`/conversations/${id}/messages`)
    messages.value = (res.messages || []).map(rowToMessage)
  } catch {
    messages.value = []
  }
  scrollBottom()
}

async function removeConversation(id) {
  if (!window.confirm('Delete this conversation? This cannot be undone.')) return
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
      toast(body, ok)
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
      toast(ev.message || 'Agent error', false)
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
  inputEl.value.style.height = `${Math.min(inputEl.value.scrollHeight, 220)}px`
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
  if (!currentSessionId.value) newChat()

  messages.value.push({ role: 'user', text: msg })
  input.value = ''
  disabled.value = true
  adjustInputHeight()

  // Add trace block then agent bubble
  messages.value.push({ role: 'trace', trace: [] })
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
  resetRunTrackers()
  disabled.value = false
}

function onKeydown(e) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    send()
  }
}

function onWindowKeydown(e) {
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
  await loadConversations()

  // Reopen the last thread if it still exists so a refresh doesn't lose the
  // user's place; otherwise start a fresh conversation.
  const last = localStorage.getItem(LS_KEY)
  if (last && conversations.value.some((c) => c.id === last)) {
    currentSessionId.value = last
    try {
      const res = await get(`/conversations/${last}/messages`)
      messages.value = (res.messages || []).map(rowToMessage)
    } catch {
      messages.value = []
    }
  } else {
    newChat()
  }

  nextTick(() => {
    inputEl.value?.focus()
    adjustInputHeight()
    scrollBottom()
  })
})

onUnmounted(() => {
  window.removeEventListener('keydown', onWindowKeydown)
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
    resetRunTrackers()
    disabled.value = false
  }
})
watch(input, () => nextTick(adjustInputHeight))
watch(disabled, (newVal) => {
  if (!newVal) {
    setTimeout(() => {
      inputEl.value?.focus()
      adjustInputHeight()
    }, 10)
  }
})
</script>

<template>
  <div class="chat-workspace">
    <aside class="conv-pane">
      <button class="conv-new" type="button" @click="newChat" title="Start a new conversation">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
          <path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
        <span>New chat</span>
      </button>

      <div class="conv-list">
        <p v-if="conversations.length === 0" class="conv-empty">No conversations yet.</p>
        <div
          v-for="c in conversations"
          :key="c.id"
          class="conv-item"
          :class="{ active: c.id === currentSessionId }"
          @click="openConversation(c.id)"
        >
          <input
            v-if="renamingId === c.id"
            ref="renameEl"
            class="conv-rename"
            v-model="renameText"
            maxlength="60"
            @click.stop
            @dblclick.stop
            @mousedown.stop
            @keydown.enter.prevent="commitRename(c)"
            @keydown.esc.prevent="cancelRename"
            @blur="commitRename(c)"
          />
          <span
            v-else
            class="conv-title"
            @dblclick.stop="startRename(c)"
            title="Double-click to rename"
          >{{ c.title || 'New chat' }}</span>
          <button
            class="conv-del"
            type="button"
            @click.stop="removeConversation(c.id)"
            title="Delete conversation"
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
              <path d="M4 7h16M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </button>
        </div>
      </div>
    </aside>

    <div class="chat-layout">
      <div class="chat-messages" ref="messagesEl">
        <div v-if="messages.length === 0" class="chat-welcome">
          <div class="chat-welcome-mark">
            <img src="/favicon.png" alt="Axon" class="logo-img chat-logo" />
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

        <template v-for="(msg, idx) in messages" :key="idx">
          <div v-if="msg.role === 'trace'" v-show="msg.trace.length > 0" class="tool-trace">
            <div v-for="(item, i) in msg.trace" :key="i" class="tool-trace-item">
              <span :style="{ color: item.color }">{{ item.text }}</span>
            </div>
          </div>

          <div v-else-if="msg.role === 'user'" class="chat-msg user">
            <div class="chat-bubble">{{ msg.text }}</div>
          </div>

          <div v-else-if="msg.role === 'agent'" class="chat-msg agent">
            <div class="chat-bubble">
              <span v-if="msg.thinking" class="thinking-indicator">{{ msg.status || 'Thinking...' }}</span>
              <span class="chat-markdown" v-html="renderMarkdown(msg.text)"></span>
            </div>
            <div v-if="msg.meta" class="chat-meta">{{ msg.meta }}</div>
          </div>
        </template>
      </div>

      <div class="chat-input-area">
        <div class="chat-input-floating">
          <textarea
            class="chat-textarea premium-input"
            ref="inputEl"
            v-model="input"
            @keydown="onKeydown"
            :disabled="disabled"
            placeholder="Message Axon..."
            rows="1"
          ></textarea>
          <button
            class="btn-chat-send"
            :class="{ 'is-stop': disabled }"
            @click="disabled ? stop() : send()"
            :disabled="!disabled && !input.trim()"
            :title="disabled ? 'Stop (Esc)' : 'Send (Enter)'"
          >
            <svg v-if="!disabled" width="18" height="18" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M22 2L11 13" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
              <path d="M22 2L15 22L11 13L2 9L22 2Z" fill="currentColor" opacity="0.4" />
              <path d="M22 2L15 22L11 13L2 9L22 2Z" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
            <svg v-else width="16" height="16" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
              <rect x="6" y="6" width="12" height="12" rx="2.5" fill="currentColor" />
            </svg>
          </button>
        </div>
        <div class="chat-hints">
          <span class="hint">Enter to send</span>
          <span class="hint">Shift+Enter for a new line</span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
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

.conv-title {
  flex: 1;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-size: 0.88rem;
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
