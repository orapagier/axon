<script setup>
import { ref, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { connectWs, wsSend, wsStatus } from '../lib/ws.js'
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

const SESSION_ID = 'owner'

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
      currentRunId = null
      agentIdx = -1
      traceIdx = -1
      disabled.value = false
      break

    case 'error':
      if (ev.run_id !== currentRunId) break
      if (agentIdx >= 0) {
        messages.value[agentIdx].thinking = false
        messages.value[agentIdx].status = ''
      }
      toast(ev.message || 'Agent error', false)
      currentRunId = null
      agentIdx = -1
      traceIdx = -1
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

  messages.value.push({ role: 'user', text: msg })
  input.value = ''
  disabled.value = true
  adjustInputHeight()

  // Add trace block then agent bubble
  messages.value.push({ role: 'trace', trace: [] })
  messages.value.push({ role: 'agent', text: '', thinking: true, meta: '', status: 'Thinking...' })

  traceIdx = messages.value.length - 2
  agentIdx = messages.value.length - 1

  await scrollBottom()
  const sent = wsSend({ task: msg, session_id: SESSION_ID })
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
  wsSend({ type: 'cancel', session_id: SESSION_ID })
  if (agentIdx >= 0) {
    const m = messages.value[agentIdx]
    m.thinking = false
    m.status = ''
    if (!m.text) m.text = 'Stopped.'
    m.meta = m.meta ? `${m.meta} · stopped` : 'stopped'
  }
  currentRunId = null
  agentIdx = -1
  traceIdx = -1
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

onMounted(() => {
  connectWs(handleWsEvent)
  window.addEventListener('keydown', onWindowKeydown)
  nextTick(() => {
    inputEl.value?.focus()
    adjustInputHeight()
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
    currentRunId = null
    agentIdx = -1
    traceIdx = -1
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
</template>
