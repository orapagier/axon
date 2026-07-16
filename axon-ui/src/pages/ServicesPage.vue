<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { get, post, del, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import Modal from '../components/Modal.vue'

const PLATFORMS = [
  {
    id: 'telegram',
    name: 'Telegram',
    icon: `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="12" fill="#24A1DE"/><path d="M12.4 14.5L14.7 16.3C15 16.5 15.3 16.4 15.4 16L16.8 9.3C16.9 8.8 16.5 8.5 16.1 8.7L7.1 12.2C6.6 12.4 6.6 12.8 7.2 13L9.5 13.7L14.8 10.3C15.1 10.1 15.3 10.2 15.1 10.4L10.8 14.3L10.6 16.8C10.8 16.8 10.9 16.7 11 16.6L12.4 15.2L12.4 14.5Z" fill="white"/></svg>`,
    key: 'messaging.telegram_token',
    desc: 'Create a bot via @BotFather.'
  },
  {
    id: 'discord',
    name: 'Discord',
    icon: `<svg viewBox="0 0 24 24" fill="#5865F2" xmlns="http://www.w3.org/2000/svg"><path d="M19.27 4.57c-1.24-.57-2.58-.99-3.97-1.23a.06.06 0 00-.06.03c-.17.3-.37.71-.5 1.02a13.1 13.1 0 00-5.48 0c-.13-.31-.33-.72-.5-1.02a.06.06 0 00-.06-.03c-1.39.24-2.73.66-3.97 1.23a.06.06 0 00-.02.02c-2.49 3.73-3.17 7.37-2.83 10.95a.06.06 0 00.02.04c1.65 1.21 3.25 1.95 4.82 2.43a.06.06 0 00.07-.02c.38-.52.73-1.08 1.02-1.68a.06.06 0 00-.03-.08c-.52-.19-1.01-.43-1.48-.71a.06.06 0 01-.01-.1c.1-.07.2-.15.3-.22a.05.05 0 01.06-.01c3.48 1.59 7.24 1.59 10.67 0a.05.05 0 01.06.01c.1.07.2.15.3.22a.06.06 0 01-.01.1c-.46.28-.96.52-1.48.71a.06.06 0 00-.03.08c.3.6.64 1.16 1.02 1.68a.06.06 0 00.07.02c1.57-.48 3.17-1.22 4.82-2.43a.06.06 0 00.02-.04c.4-4.14-.67-7.74-2.83-10.95a.06.06 0 00-.02-.02zM8.52 13.19c-.77 0-1.41-.7-1.41-1.56s.62-1.56 1.41-1.56c.79 0 1.43.7 1.41 1.56-.02.86-.62 1.56-1.41 1.56zm6.96 0c-.77 0-1.41-.7-1.41-1.56s.62-1.56 1.41-1.56c.79 0 1.43.7 1.43 1.56 0 .86-.62 1.56-1.43 1.56z"/></svg>`,
    key: 'messaging.discord_token',
    desc: 'Discord Developer Portal.'
  },
  {
    id: 'slack',
    name: 'Slack',
    icon: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M5.042 15.165a2.528 2.528 0 01-2.52 2.523A2.528 2.528 0 010 15.165a2.527 2.527 0 012.522-2.52h2.52v2.52zM6.313 15.165a2.527 2.527 0 012.521-2.52 2.527 2.527 0 012.521 2.52v6.313A2.528 2.528 0 018.834 24a2.528 2.528 0 01-2.521-2.522v-6.313zM8.834 5.042a2.528 2.528 0 01-2.523-2.52A2.528 2.528 0 018.834 0a2.527 2.527 0 012.52 2.522v2.52h-2.52zM8.834 6.313a2.527 2.527 0 012.52 2.521 2.527 2.527 0 01-2.52 2.521H2.522A2.528 2.528 0 010 8.834a2.528 2.528 0 012.522-2.521h6.312zM18.958 8.834a2.528 2.528 0 012.522-2.522A2.528 2.528 0 0124 8.834a2.527 2.527 0 01-2.522 2.521h-2.52V8.834zM17.687 8.834a2.527 2.527 0 01-2.521 2.521 2.527 2.527 0 01-2.521-2.521V2.522A2.528 2.528 0 0115.166 0a2.528 2.528 0 012.521 2.522v6.312zM15.166 18.958a2.528 2.528 0 012.523 2.522A2.528 2.528 0 0115.166 24a2.527 2.527 0 01-2.52-2.522v-2.52h2.52zM15.166 17.687a2.527 2.527 0 01-2.52-2.521 2.527 2.527 0 012.52-2.521h6.312A2.528 2.528 0 0124 15.166a2.528 2.528 0 01-2.522 2.521h-6.312z" fill="#36C5F0"/></svg>`,
    key: 'messaging.slack_token',
    desc: 'Bot User OAuth Token.'
  },
]

const AUTH_METADATA = {
  google: {
    name: 'Google',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z"/><path fill="#4285F4" d="M46.64 24.22c0-1.63-.15-3.2-.42-4.72H24v9.01h12.75c-.53 2.85-2.13 5.27-4.52 6.9l7.35 5.7c4.35-4 6.94-9.88 7.06-16.89z"/><path fill="#34A853" d="M9.64 28.77c-1.12-3.34-1.12-6.95 0-10.29l-7.98-6.19C-.41 15.69-.41 23.31 1.66 28.77l7.98-6.19z"/><path fill="#FBBC05" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.35-5.7c-2.31 1.54-5.22 2.45-8.54 2.45-6.26 0-11.57-4.22-13.47-9.9l-7.98 6.19C6.51 42.62 14.62 48 24 48z"/></svg>`,
  },
  microsoft: {
    name: 'Microsoft',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 23 23"><path fill="#f35325" d="M1 1h10v10H1z"/><path fill="#81bc06" d="M12 1h10v10H12z"/><path fill="#05a6f0" d="M1 12h10v10H1z"/><path fill="#ffba08" d="M12 12h10v10H12z"/></svg>`,
  },
  facebook: {
    name: 'Facebook',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="12" fill="#1877F2"/><path d="M15 12h-2.5v9h-3.5v-9h-2v-3h2v-2a3.5 3.5 0 0 1 3.5-3.5h2.5v3h-1.5c-0.8 0-1 0.4-1 1v1.5h2.5l-0.5 3Z" fill="#FFFFFF"/></svg>`,
  },
  instagram: {
    name: 'Instagram',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><defs><radialGradient id="ig-grad-auth-official" cx="0.25" cy="0.9" r="1.3"><stop offset="0.05" stop-color="#fedd75"/><stop offset="0.2" stop-color="#f7a739"/><stop offset="0.3" stop-color="#e15042"/><stop offset="0.5" stop-color="#d32e7d"/><stop offset="0.75" stop-color="#9b36b7"/><stop offset="1" stop-color="#515ecf"/></radialGradient></defs><rect width="24" height="24" rx="6" fill="url(#ig-grad-auth-official)"/><path d="M16 3H8C5.23 3 3 5.23 3 8v8c0 2.77 2.23 5 5 5h8c2.77 0 5-2.23 5-5V8c0-2.77-2.23-5-5-5zm3 13c0 1.66-1.34 3-3 3H8c-1.66 0-3-1.34-3-3V8c0-1.66 1.34-3 3-3h8c1.66 0 3 1.34 3 3v8z" fill="white"/><circle cx="12" cy="12" r="4.5" fill="none" stroke="white" stroke-width="1.8"/><circle cx="17" cy="7" r="1.2" fill="white"/></svg>`,
  }
}

// Neutral single-stroke glyphs for the non-brand tiles — currentColor so they
// sit quietly in the graphite palette.
const GLYPHS = {
  key: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="M10.85 12.15 19 4M18 5l2 2M15 8l2 2"/></svg>`,
  plug: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9 7V3M15 7V3M7 7h10v4a5 5 0 0 1-10 0V7zM12 16v5"/></svg>`,
  terminal: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/></svg>`,
  search: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="6"/><path d="m20 20-4.8-4.8"/></svg>`,
}

const mcpServers = ref([])
const mcpTools = ref([])
const sshServers = ref([])
const wsAccounts = ref([])
const authStatus = ref({})

const messagingSettings = ref([])
const messagingStatus = ref({})
const messagingTokens = ref({ telegram: '', discord: '', slack: '' })
let messagingPollInterval

const credentials = ref([])
const credModal = ref(false)
const credForm = ref({ name: '', service: 'telegram', fields: [{ key: 'access_token', value: '' }] })
const testingCred = ref(null)

const mcpModal = ref(false)
const sshModal = ref(false)
const wsModal = ref(false)
const msgModal = ref(false)
const msgPlatform = ref(null)
const fbAppModal = ref(false)
const fbAppForm = ref({ app_id: '', app_secret: '', app_secret_set: false, verify_token: '', page_id: '' })

const mcpForm = ref({ name: '', url: '', api_key: '' })
const sshForm = ref({
  name: '',
  ip: '',
  port: 22,
  username: '',
  auth_type: 'key',
  private_key: '',
  public_key: '',
  password: '',
})
const wsForm = ref({ id: '', name: '', api_key: '', priority: 1, enabled: true })
const wsEditing = ref(false)

const connectedAuthCount = computed(
  () => Object.keys(AUTH_METADATA).filter((p) => authStatus.value[p]?.authenticated).length
)
const connectedMessagingCount = computed(
  () => PLATFORMS.filter((p) => isMessagingConnected(p)).length
)
const liveCount = computed(
  () =>
    mcpServers.value.length +
    connectedMessagingCount.value +
    connectedAuthCount.value +
    wsAccounts.value.filter((a) => a.enabled).length
)
const configuredCount = computed(
  () =>
    credentials.value.length +
    mcpServers.value.length +
    sshServers.value.length +
    wsAccounts.value.length +
    PLATFORMS.filter((p) => isMessagingConfigured(p)).length +
    connectedAuthCount.value
)

async function load() {
  await Promise.all([loadMcp(), loadSsh(), loadWs(), loadAuth(), loadMessaging(), loadCredentials()])
}

async function loadCredentials() {
  const d = await get('/credentials')
  credentials.value = d.credentials || []
}

async function loadMcp() {
  const d = await get('/mcp')
  mcpServers.value = d.servers || []
  mcpTools.value = d.tools || []
}

async function loadSsh() {
  const d = await get('/ssh_servers')
  sshServers.value = d.servers || []
}

async function loadWs() {
  const d = await get('/websearch/accounts')
  wsAccounts.value = d.accounts || []
}

async function loadAuth() {
  const d = await get('/integrations/status')
  authStatus.value = d.auth_status || {}
}

function mcpToolCount(name) {
  return mcpTools.value.filter((t) => t.source?.server_name === name).length
}

// MCP
async function connectMcp() {
  if (!mcpForm.value.name || !mcpForm.value.url) return toast('Name and URL required', false)
  const r = await post('/mcp', mcpForm.value)
  toast(r.ok ? `Connected (${r.tool_count} tools)` : r.error, r.ok)
  if (r.ok) {
    mcpModal.value = false
    mcpForm.value = { name: '', url: '', api_key: '' }
    loadMcp()
  }
}
async function disconnectMcp(name) {
  const r = await del(`/mcp/${encodeURIComponent(name)}`)
  toast(r.ok ? 'Disconnected' : r.error, r.ok)
  loadMcp()
}

// SSH
async function saveSsh() {
  if (!sshForm.value.name || !sshForm.value.ip || !sshForm.value.username) {
    return toast('Name, IP, and Username required', false)
  }
  const r = await post('/ssh_servers', {
    ...sshForm.value,
    port: Number(sshForm.value.port),
  })
  toast(r.ok ? 'Server saved' : r.error, r.ok)
  if (r.ok) {
    sshModal.value = false
    loadSsh()
  }
}
async function deleteSsh(name) {
  const ok = await confirmDialog(`"${name}" will be permanently removed from your servers.`, {
    title: 'Delete Server',
    confirmText: 'Delete',
  })
  if (!ok) return
  await del(`/ssh_servers/${encodeURIComponent(name)}`)
  loadSsh()
}

// Web Search
function showAddWs() {
  wsEditing.value = false
  wsForm.value = { id: '', name: '', api_key: '', priority: 1, enabled: true }
  wsModal.value = true
}
function showEditWs(a) {
  wsEditing.value = true
  wsForm.value = {
    id: a.id,
    name: a.name,
    api_key: a.api_key_preview,
    priority: a.priority,
    enabled: a.enabled,
  }
  wsModal.value = true
}
async function saveWs() {
  if (!wsForm.value.name || !wsForm.value.api_key) {
    return toast('Name and API Key required', false)
  }
  const r = await post('/websearch/accounts', {
    ...wsForm.value,
    id: wsForm.value.id || null,
  })
  toast(r.ok ? 'Account saved' : r.error, r.ok)
  if (r.ok) {
    wsModal.value = false
    loadWs()
  }
}
async function deleteWs(id) {
  const ok = await confirmDialog('This search account will be permanently removed.', {
    title: 'Delete Search Account',
    confirmText: 'Delete',
  })
  if (!ok) return
  const r = await del(`/websearch/accounts/${id}`)
  toast(r.ok ? 'Deleted' : r.error, r.ok)
  loadWs()
}
async function resetWsQuotas() {
  const ok = await confirmDialog('All monthly search counters will be reset to zero.', {
    title: 'Reset Quotas',
    confirmText: 'Reset',
    danger: false,
  })
  if (!ok) return
  const r = await post('/websearch/reset', {})
  toast(r.ok ? 'Quotas reset' : r.error, r.ok)
  loadWs()
}

// Auth
async function connectAuth(p) {
  const r = await post(`/integrations/${p}/url`, {})
  if (r.url || r.login_url) window.open(r.url || r.login_url, '_blank')
  else toast(`Failed to get auth URL: ${r.error || JSON.stringify(r)}`, false)
}
async function openFbAppModal() {
  const d = await get('/facebook/app-credentials')
  fbAppForm.value = {
    app_id: d.app_id || '',
    app_secret: '',
    app_secret_set: !!d.app_secret_set,
    verify_token: d.verify_token || '',
    page_id: d.page_id || '',
  }
  fbAppModal.value = true
}

async function saveFbAppCredentials() {
  if (!fbAppForm.value.app_id || !fbAppForm.value.verify_token || !fbAppForm.value.page_id) {
    return toast('App ID, Verify Token, and Page ID are required', false)
  }
  const r = await put('/facebook/app-credentials', {
    app_id: fbAppForm.value.app_id,
    app_secret: fbAppForm.value.app_secret,
    verify_token: fbAppForm.value.verify_token,
    page_id: fbAppForm.value.page_id,
  })
  toast(r.ok ? 'Facebook App credentials saved' : (r.error || 'Failed to save Facebook App credentials'), !!r.ok)
  if (r.ok) fbAppModal.value = false
}

async function disconnectAuth(p) {
  const ok = await confirmDialog(`You can reconnect ${p} again later.`, {
    title: `Disconnect ${p}`,
    confirmText: 'Disconnect',
  })
  if (!ok) return
  const r = await post(`/integrations/${p}/disconnect`, {})
  toast(r.success ? `${p} disconnected` : r.error || 'Failed', !!r.success)
  loadAuth()
}

// Credentials
async function saveCredential() {
  if (!credForm.value.name || !credForm.value.service) {
    return toast('Name and Service required', false)
  }
  const parsed = {}
  for (const field of credForm.value.fields || []) {
    if (field.key.trim()) {
      parsed[field.key.trim()] = field.value
    }
  }
  if (Object.keys(parsed).length === 0) {
    return toast('Add at least one credential field', false)
  }

  const r = await post('/credentials', {
    name: credForm.value.name,
    service: credForm.value.service,
    data: parsed
  })
  toast(r.ok ? 'Credential saved' : r.error, r.ok)
  if (r.ok) {
    credModal.value = false
    loadCredentials()
  }
}

async function deleteCredential(id) {
  const ok = await confirmDialog('This credential will be permanently deleted.', {
    title: 'Delete Credential',
    confirmText: 'Delete',
  })
  if (!ok) return
  const r = await del(`/credentials/${id}`)
  toast(r.ok ? 'Deleted' : r.error, r.ok)
  loadCredentials()
}

async function testCredential(id) {
  testingCred.value = id
  try {
    const r = await post(`/credentials/${id}/test`, {})
    const msg = r.ok
      ? (r.tested ? (r.message || 'Credential is valid') : (r.message || 'Credential present (not testable)'))
      : (r.error || 'Credential test failed')
    toast(msg, r.ok)
  } finally {
    testingCred.value = null
  }
}

// Messaging
async function loadMessaging() {
  const [s, st] = await Promise.all([get('/settings'), get('/messaging/status')])
  messagingSettings.value = s.settings || []
  messagingStatus.value = st || {}

  PLATFORMS.forEach(p => {
    const s2 = messagingSettings.value.find(x => x.key === p.key)
    if (s2 && !messagingTokens.value[p.id]) {
      messagingTokens.value[p.id] = s2.value
    }
  })
}

function openMsgModal(p) {
  msgPlatform.value = p
  msgModal.value = true
}

async function saveMessagingToken(p) {
  const r = await put(`/settings/${encodeURIComponent(p.key)}`, { value: messagingTokens.value[p.id] })
  toast(r.ok ? `Saved ${p.name} token` : r.error, r.ok)
  if (r.ok) msgModal.value = false
  loadMessaging()
}

async function reconnectMessaging(p) {
  toast(`Starting ${p.name}...`, true)
  const r = await post(`/messaging/reconnect/${p.id}`)
  toast(r.ok ? `${p.name} started!` : r.error || `Failed to start ${p.name}`, r.ok)
  loadMessaging()
}

function isMessagingConfigured(p) {
  const s = messagingSettings.value.find(x => x.key === p.key)
  return !!(s && s.value)
}

function isMessagingConnected(p) {
  return messagingStatus.value[p.id]?.connected
}

onMounted(() => {
  load()
  messagingPollInterval = setInterval(loadMessaging, 5000)
})

onUnmounted(() => {
  clearInterval(messagingPollInterval)
})
</script>

<template>
  <div class="page-wrap services-page">
    <div class="page-toolbar">
      <p class="page-readout">
        <span class="readout-em">{{ liveCount }}</span> live ·
        <span class="readout-em">{{ configuredCount }}</span> configured
      </p>
      <button
        class="btn btn-ghost"
        @click="load"
      >
        Refresh
      </button>
    </div>

    <!-- Messaging -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          Messaging
        </h2>
        <span class="svc-group-count">{{ connectedMessagingCount }}/{{ PLATFORMS.length }} active</span>
      </header>
      <div class="svc-grid">
        <article
          v-for="p in PLATFORMS"
          :key="p.id"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon"
              v-html="p.icon"
            />
            <span class="svc-name">{{ p.name }}</span>
            <span
              class="svc-status"
              :class="isMessagingConnected(p) ? 'ok' : (isMessagingConfigured(p) ? 'warm' : 'off')"
            >{{ isMessagingConnected(p) ? 'active' : (isMessagingConfigured(p) ? 'ready' : 'offline') }}</span>
          </div>
          <p class="svc-meta">
            {{ p.desc }}
          </p>
          <div class="svc-actions">
            <button
              class="btn btn-ghost"
              @click="openMsgModal(p)"
            >
              Token
            </button>
            <button
              class="btn btn-ghost"
              :disabled="!isMessagingConfigured(p)"
              @click="reconnectMessaging(p)"
            >
              {{ isMessagingConnected(p) ? 'Restart' : 'Connect' }}
            </button>
          </div>
        </article>
      </div>
    </section>

    <!-- Accounts (OAuth) -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          Accounts
        </h2>
        <span class="svc-group-count">{{ connectedAuthCount }}/{{ Object.keys(AUTH_METADATA).length }} connected</span>
      </header>
      <div class="svc-grid">
        <article
          v-for="(meta, id) in AUTH_METADATA"
          :key="id"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon"
              v-html="meta.icon"
            />
            <span class="svc-name">{{ meta.name }}</span>
            <span
              class="svc-status"
              :class="authStatus[id]?.authenticated ? 'ok' : 'off'"
            >{{ authStatus[id]?.authenticated ? 'connected' : 'off' }}</span>
          </div>
          <p class="svc-meta">
            {{ authStatus[id]?.authenticated
              ? (authStatus[id]?.user || authStatus[id]?.email || 'Session active')
              : `OAuth sign-in for ${meta.name}` }}
          </p>
          <div class="svc-actions">
            <button
              v-if="id === 'facebook'"
              class="btn btn-ghost"
              @click="openFbAppModal"
            >
              Edit App
            </button>
            <button
              v-if="!authStatus[id]?.authenticated"
              class="btn btn-ghost"
              @click="connectAuth(id)"
            >
              Connect
            </button>
            <button
              v-else
              class="btn btn-ghost svc-danger"
              @click="disconnectAuth(id)"
            >
              Disconnect
            </button>
          </div>
        </article>
      </div>
    </section>

    <!-- MCP -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          MCP Servers
        </h2>
        <span class="svc-group-count">{{ mcpServers.length }} connected</span>
      </header>
      <div class="svc-grid">
        <article
          v-for="name in mcpServers"
          :key="name"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon glyph"
              v-html="GLYPHS.plug"
            />
            <span class="svc-name">{{ name }}</span>
            <span class="svc-status ok">connected</span>
          </div>
          <p class="svc-meta">
            {{ mcpToolCount(name) }} dynamic tool{{ mcpToolCount(name) === 1 ? '' : 's' }}
          </p>
          <div class="svc-actions">
            <button
              class="btn btn-ghost svc-danger"
              @click="disconnectMcp(name)"
            >
              Disconnect
            </button>
          </div>
        </article>
        <button
          class="svc-add-tile"
          type="button"
          @click="mcpModal = true"
        >
          + Connect MCP server
        </button>
      </div>
    </section>

    <!-- SSH -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          SSH Servers
        </h2>
        <span class="svc-group-count">{{ sshServers.length }} registered</span>
      </header>
      <div class="svc-grid">
        <article
          v-for="s in sshServers"
          :key="s.name"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon glyph"
              v-html="GLYPHS.terminal"
            />
            <span class="svc-name">{{ s.name }}</span>
            <span class="svc-status warm">{{ s.auth_type }}</span>
          </div>
          <p class="svc-meta mono">
            {{ s.username }}@{{ s.ip }}:{{ s.port }}
          </p>
          <div class="svc-actions">
            <button
              class="btn btn-ghost svc-danger"
              @click="deleteSsh(s.name)"
            >
              Remove
            </button>
          </div>
        </article>
        <button
          class="svc-add-tile"
          type="button"
          @click="sshModal = true"
        >
          + Register SSH server
        </button>
      </div>
    </section>

    <!-- Web Search -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          Web Search
        </h2>
        <span class="svc-group-count">{{ wsAccounts.filter((a) => a.enabled).length }} active</span>
        <button
          v-if="wsAccounts.length"
          class="svc-group-action"
          type="button"
          @click="resetWsQuotas"
        >
          Reset quotas
        </button>
      </header>
      <div class="svc-grid">
        <article
          v-for="a in wsAccounts"
          :key="a.id"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon glyph"
              v-html="GLYPHS.search"
            />
            <span class="svc-name">{{ a.name }}</span>
            <span
              class="svc-status"
              :class="a.enabled ? 'ok' : 'err'"
            >{{ a.enabled ? 'enabled' : 'exhausted' }}</span>
          </div>
          <p class="svc-meta mono">
            priority {{ a.priority }} · {{ a.queries_this_month }} queries
          </p>
          <div class="svc-actions">
            <button
              class="btn btn-ghost"
              @click="showEditWs(a)"
            >
              Edit
            </button>
            <button
              class="btn btn-ghost svc-danger"
              @click="deleteWs(a.id)"
            >
              Delete
            </button>
          </div>
        </article>
        <button
          class="svc-add-tile"
          type="button"
          @click="showAddWs"
        >
          + Add Tavily account
        </button>
      </div>
    </section>

    <!-- Credentials -->
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          Credentials
        </h2>
        <span class="svc-group-count">{{ credentials.length }} stored</span>
      </header>
      <div class="svc-grid">
        <article
          v-for="c in credentials"
          :key="c.id"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon glyph"
              v-html="GLYPHS.key"
            />
            <span class="svc-name">{{ c.name }}</span>
            <span class="svc-status warm">{{ c.service }}</span>
          </div>
          <p class="svc-meta">
            Encrypted at rest.
          </p>
          <div class="svc-actions">
            <button
              class="btn btn-ghost"
              :disabled="testingCred === c.id"
              @click="testCredential(c.id)"
            >
              {{ testingCred === c.id ? 'Testing…' : 'Test' }}
            </button>
            <button
              class="btn btn-ghost svc-danger"
              @click="deleteCredential(c.id)"
            >
              Delete
            </button>
          </div>
        </article>
        <button
          class="svc-add-tile"
          type="button"
          @click="credModal = true"
        >
          + Add secure credential
        </button>
      </div>
    </section>
  </div>

  <!-- MODALS -->

  <!-- Messaging token -->
  <Modal
    v-model="msgModal"
    :title="msgPlatform ? `${msgPlatform.name} token` : 'Bot token'"
  >
    <div
      v-if="msgPlatform"
      class="mform"
    >
      <div class="mform-field">
        <label>Bot Token</label>
        <input
          v-model="messagingTokens[msgPlatform.id]"
          type="password"
          placeholder="Paste bot token…"
        >
        <p class="mform-hint">
          {{ msgPlatform.desc }}
        </p>
      </div>
      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="msgModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="saveMessagingToken(msgPlatform)"
        >
          Save Token
        </button>
      </div>
    </div>
  </Modal>

  <!-- Facebook App credentials -->
  <Modal
    v-model="fbAppModal"
    title="Facebook App Credentials"
  >
    <div class="mform">
      <div class="mform-field">
        <label>App ID</label>
        <input
          v-model="fbAppForm.app_id"
          type="text"
          placeholder="e.g. 920903276972742"
        >
      </div>
      <div class="mform-field">
        <label>App Secret</label>
        <input
          v-model="fbAppForm.app_secret"
          type="password"
          :placeholder="fbAppForm.app_secret_set ? '•••••••••••••••• (unchanged)' : 'Paste app secret…'"
        >
        <p class="mform-hint">
          Leave blank to keep the currently stored secret.
        </p>
      </div>
      <div class="mform-field">
        <label>Verify Token</label>
        <input
          v-model="fbAppForm.verify_token"
          type="text"
          placeholder="Any string you also set in the Meta webhook config"
        >
      </div>
      <div class="mform-field">
        <label>Page ID</label>
        <input
          v-model="fbAppForm.page_id"
          type="text"
          placeholder="e.g. 846232565231219"
        >
      </div>
      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="fbAppModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="saveFbAppCredentials"
        >
          Save
        </button>
      </div>
    </div>
  </Modal>

  <!-- MCP -->
  <Modal
    v-model="mcpModal"
    title="Connect MCP Server"
  >
    <div class="mform">
      <div class="mform-field">
        <label>Name</label>
        <input
          v-model="mcpForm.name"
          type="text"
          placeholder="e.g. Local Tools"
        >
      </div>
      <div class="mform-field">
        <label>URL</label>
        <input
          v-model="mcpForm.url"
          type="text"
          placeholder="http://localhost:8000"
        >
      </div>
      <div class="mform-field">
        <label>API Key / Token (optional)</label>
        <input
          v-model="mcpForm.api_key"
          type="password"
          placeholder="••••••••••••••••"
        >
      </div>
      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="mcpModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="connectMcp"
        >
          Connect
        </button>
      </div>
    </div>
  </Modal>

  <!-- SSH -->
  <Modal
    v-model="sshModal"
    title="Add SSH Server"
  >
    <div class="mform">
      <div class="mform-field">
        <label>Server Name</label>
        <input
          v-model="sshForm.name"
          type="text"
          placeholder="e.g. prod-db-1"
        >
      </div>

      <div class="mform-row">
        <div class="mform-field grow-2">
          <label>IP / Hostname</label>
          <input
            v-model="sshForm.ip"
            type="text"
            placeholder="192.168.1.1"
          >
        </div>
        <div class="mform-field grow-1">
          <label>Port</label>
          <input
            v-model="sshForm.port"
            type="number"
          >
        </div>
      </div>

      <div class="mform-row">
        <div class="mform-field grow-1">
          <label>Username</label>
          <input
            v-model="sshForm.username"
            type="text"
            placeholder="root"
          >
        </div>
        <div class="mform-field grow-1">
          <label>Auth Type</label>
          <select v-model="sshForm.auth_type">
            <option value="key">
              SSH Key Pair
            </option>
            <option value="password">
              Password
            </option>
          </select>
        </div>
      </div>

      <template v-if="sshForm.auth_type === 'key'">
        <div class="mform-field">
          <label>Private Key (PEM/OpenSSH)</label>
          <textarea
            v-model="sshForm.private_key"
            class="mform-code"
            rows="4"
            placeholder="-----BEGIN OPENSSH PRIVATE KEY..."
          />
        </div>
        <div class="mform-field">
          <label>Public Key (optional)</label>
          <textarea
            v-model="sshForm.public_key"
            class="mform-code"
            rows="2"
          />
        </div>
      </template>

      <div
        v-else
        class="mform-field"
      >
        <label>Password</label>
        <input
          v-model="sshForm.password"
          type="password"
          placeholder="••••••••••••••••"
        >
      </div>

      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="sshModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="saveSsh"
        >
          Save Server
        </button>
      </div>
    </div>
  </Modal>

  <!-- Web Search -->
  <Modal
    v-model="wsModal"
    :title="wsEditing ? 'Edit Account' : 'Add Tavily Account'"
  >
    <div class="mform">
      <div class="mform-field">
        <label>Account Name</label>
        <input
          v-model="wsForm.name"
          type="text"
          placeholder="e.g. Tavily Personal"
        >
      </div>
      <div class="mform-field">
        <label>Tavily API Key</label>
        <input
          v-model="wsForm.api_key"
          type="password"
          placeholder="tvly-..."
        >
      </div>
      <div class="mform-row align-end">
        <div class="mform-field grow-1">
          <label>Priority</label>
          <input
            v-model="wsForm.priority"
            type="number"
            min="1"
          >
        </div>
        <div class="mform-field grow-1 mform-switch-row">
          <label>Enabled</label>
          <button
            class="switch"
            type="button"
            role="switch"
            :aria-checked="wsForm.enabled ? 'true' : 'false'"
            aria-label="Toggle enabled"
            @click="wsForm.enabled = !wsForm.enabled"
          />
        </div>
      </div>
      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="wsModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="saveWs"
        >
          Save Account
        </button>
      </div>
    </div>
  </Modal>

  <!-- Credential -->
  <Modal
    v-model="credModal"
    title="Add Credential"
  >
    <div class="mform">
      <div class="mform-field">
        <label>Name</label>
        <input
          v-model="credForm.name"
          type="text"
          placeholder="e.g. My Prod Bot Token"
        >
      </div>
      <div class="mform-field">
        <label>Service (Platform)</label>
        <input
          v-model="credForm.service"
          type="text"
          placeholder="e.g. telegram"
        >
      </div>
      <div class="mform-field">
        <label>Fields</label>
        <div
          v-for="(field, index) in credForm.fields"
          :key="index"
          class="mform-kv"
        >
          <input
            v-model="field.key"
            type="text"
            placeholder="Key (e.g. access_token)"
            class="mform-kv-key"
          >
          <!-- password, not text: these fields hold credential secrets
               (access_token, api_key, private_key, ...) and were previously
               rendered in plaintext while being typed. -->
          <input
            v-model="field.value"
            type="password"
            placeholder="Value"
            class="mform-kv-val"
          >
          <button
            class="btn btn-ghost btn-icon"
            title="Remove field"
            @click="credForm.fields.splice(index, 1)"
          >
            ✕
          </button>
        </div>
        <button
          class="btn btn-ghost mform-add-field"
          @click="credForm.fields.push({ key: '', value: '' })"
        >
          + Add field
        </button>
      </div>
      <div class="mform-actions">
        <button
          class="btn btn-ghost"
          @click="credModal = false"
        >
          Cancel
        </button>
        <button
          class="btn btn-save"
          @click="saveCredential"
        >
          Save
        </button>
      </div>
    </div>
  </Modal>
</template>

<style scoped>
.services-page {
  padding-bottom: 60px;
}

/* ── Groups: a slim label riding a hairline, no boxes around boxes ────────── */
.svc-group {
  margin-bottom: 26px;
}

.svc-group-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding-bottom: 8px;
  margin-bottom: 12px;
  border-bottom: 1px solid var(--border);
}

.svc-group-title {
  margin: 0;
  font-family: var(--font-display);
  font-size: 0.7rem;
  font-weight: 600;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: color-mix(in srgb, var(--text) 78%, transparent);
}

.svc-group-count {
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
}

.svc-group-action {
  margin-left: auto;
  padding: 0;
  border: 0;
  background: none;
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
  cursor: pointer;
  transition: color 0.15s ease;
}

.svc-group-action:hover {
  color: var(--accent);
}

/* ── Tile grid ────────────────────────────────────────────────────────────── */
.svc-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
  gap: 10px;
  align-items: stretch;
}

.svc-tile {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--r-lg);
  transition: border-color 0.15s ease;
}

.svc-tile:hover {
  border-color: color-mix(in srgb, var(--text) 24%, transparent);
}

.svc-tile-top {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
}

.svc-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  flex-shrink: 0;
}

.svc-icon :deep(svg) {
  width: 18px;
  height: 18px;
}

.svc-icon.glyph {
  color: var(--muted);
}

.svc-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.84rem;
  font-weight: 600;
  color: var(--text);
}

/* Status readout: a dot + lowercase mono word, no pill chrome */
.svc-status {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.02em;
  color: var(--muted);
}

.svc-status::before {
  content: "";
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: currentColor;
  opacity: 0.9;
}

.svc-status.ok {
  color: var(--green);
}

.svc-status.warm {
  color: var(--teal);
}

.svc-status.err {
  color: var(--red);
}

.svc-status.off {
  color: color-mix(in srgb, var(--muted) 70%, transparent);
}

.svc-meta {
  margin: 0;
  min-height: 2.4em;
  font-size: 0.72rem;
  line-height: 1.5;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.svc-meta.mono {
  font-family: var(--font-mono);
  font-size: 0.68rem;
}

.svc-actions {
  display: flex;
  gap: 6px;
  margin-top: auto;
}

.svc-danger:hover {
  border-color: color-mix(in srgb, var(--red) 50%, transparent) !important;
  color: var(--red) !important;
}

/* Dashed ghost tile = the group's add action */
.svc-add-tile {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 106px;
  padding: 12px;
  border: 1px dashed color-mix(in srgb, var(--text) 25%, transparent);
  border-radius: var(--r-lg);
  background: none;
  color: var(--muted);
  font: inherit;
  font-size: 0.76rem;
  cursor: pointer;
  transition: border-color 0.15s ease, color 0.15s ease, background 0.15s ease;
}

.svc-add-tile:hover {
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 4%, transparent);
}

/* ── Modal forms: label-over-field, hairline inputs, no decoration ────────── */
.mform {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.mform-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.mform-field label {
  font-family: var(--font-mono);
  font-size: 0.64rem;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}

.mform-field input,
.mform-field select,
.mform-field textarea {
  width: 100%;
  padding: 8px 10px;
  font-size: 0.8rem;
}

.mform-hint {
  margin: 0;
  font-size: 0.72rem;
  color: var(--muted);
}

.mform-code {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  line-height: 1.5;
  resize: vertical;
}

.mform-row {
  display: flex;
  gap: 12px;
}

.mform-row.align-end {
  align-items: flex-end;
}

.grow-1 {
  flex: 1;
}

.grow-2 {
  flex: 2;
}

.mform-switch-row {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  padding-bottom: 8px;
}

.mform-switch-row label {
  text-transform: none;
  font-family: inherit;
  font-size: 0.8rem;
  color: var(--text);
}

.mform-kv {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.mform-kv-key {
  flex: 1;
}

.mform-kv-val {
  flex: 2;
}

.mform-add-field {
  width: 100%;
}

.mform-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}

@media (max-width: 640px) {
  .svc-grid {
    grid-template-columns: 1fr;
  }

  .mform-row {
    flex-direction: column;
    gap: 14px;
  }
}
</style>
