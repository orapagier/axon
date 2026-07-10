<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { get, post, del, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import Modal from '../components/Modal.vue'
import Pill from '../components/Pill.vue'
import SearchInput from '../components/SearchInput.vue'

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
    color: '#4285F4'
  },
  microsoft: {
    name: 'Microsoft',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 23 23"><path fill="#f35325" d="M1 1h10v10H1z"/><path fill="#81bc06" d="M12 1h10v10H12z"/><path fill="#05a6f0" d="M1 12h10v10H1z"/><path fill="#ffba08" d="M12 12h10v10H12z"/></svg>`,
    color: '#05a6f0'
  },
  facebook: {
    name: 'Facebook',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="12" fill="#1877F2"/><path d="M15 12h-2.5v9h-3.5v-9h-2v-3h2v-2a3.5 3.5 0 0 1 3.5-3.5h2.5v3h-1.5c-0.8 0-1 0.4-1 1v1.5h2.5l-0.5 3Z" fill="#FFFFFF"/></svg>`,
    color: '#1877F2'
  },
  instagram: {
    name: 'Instagram',
    icon: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><defs><radialGradient id="ig-grad-auth-official" cx="0.25" cy="0.9" r="1.3"><stop offset="0.05" stop-color="#fedd75"/><stop offset="0.2" stop-color="#f7a739"/><stop offset="0.3" stop-color="#e15042"/><stop offset="0.5" stop-color="#d32e7d"/><stop offset="0.75" stop-color="#9b36b7"/><stop offset="1" stop-color="#515ecf"/></radialGradient></defs><rect width="24" height="24" rx="6" fill="url(#ig-grad-auth-official)"/><path d="M16 3H8C5.23 3 3 5.23 3 8v8c0 2.77 2.23 5 5 5h8c2.77 0 5-2.23 5-5V8c0-2.77-2.23-5-5-5zm3 13c0 1.66-1.34 3-3 3H8c-1.66 0-3-1.34-3-3V8c0-1.66 1.34-3 3-3h8c1.66 0 3 1.34 3 3v8z" fill="white"/><circle cx="12" cy="12" r="4.5" fill="none" stroke="white" stroke-width="1.8"/><circle cx="17" cy="7" r="1.2" fill="white"/></svg>`,
    color: '#E4405F'
  }
}

const CATEGORY_META = {
  credentials: {
    icon: `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M12 1L3 5v6c0 5.55 3.84 10.74 9 12 5.16-1.26 9-6.45 9-12V5l-9-4zm0 6c1.4 0 2.5 1.1 2.5 2.5S13.4 12 12 12s-2.5-1.1-2.5-2.5S10.6 7 12 7zm0 10c-2.3 0-4.3-1.1-5.5-2.8.03-1.8 3.7-2.7 5.5-2.7s5.47.9 5.5 2.7c-1.2 1.7-3.2 2.8-5.5 2.8z" fill="#6c5ce7"/></svg>`,
    color: '#6c5ce7',
    desc: 'Encrypted storage for sensitive access tokens.'
  },
  mcp: {
    icon: `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M16 7V3h-2v4h-4V3H8v4H6v11c0 2.21 1.79 4 4 4h4c2.21 0 4-1.79 4-4V7h-2z" fill="#a29bfe"/></svg>`,
    color: '#a29bfe',
    desc: 'Model Context Protocol server integrations.'
  },
  ssh: {
    icon: `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M20 4H4c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V6c0-1.1-.9-2-2-2zM4 18V6h16v12H4zm2-10h2v2H6V8zm0 4h2v2H6v-2zm4-4h8v2h-8V8zm0 4h5v2h-5v-2z" fill="#dfe6e9"/></svg>`,
    color: '#dfe6e9',
    desc: 'Remote server access via secure shell.'
  },
  ws: {
    icon: `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z" fill="#00cec9"/></svg>`,
    color: '#00cec9',
    desc: 'High-performance AI web search capabilities.'
  }
}

const mcpServers = ref([])
const mcpTools = ref([])
const sshServers = ref([])
const wsAccounts = ref([])
const authStatus = ref({})

const mcpSummary = ref('none')
const sshSummary = ref('none')
const wsSummary = ref('none')
const authSummary = ref('none connected')
const messagingSummary = ref('none')
const credentialsSummary = ref('none')

const messagingSettings = ref([])
const messagingStatus = ref({})
const messagingTokens = ref({ telegram: '', discord: '', slack: '' })
let messagingPollInterval

const credentials = ref([])
const credModal = ref(false)
const servicesSearch = ref('')
function byName(list, nameOf) {
  const q = servicesSearch.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((item) => nameOf(item).toLowerCase().includes(q))
}
const filteredCredentials = computed(() => byName(credentials.value, (c) => c.name))
const filteredMcpServers = computed(() => byName(mcpServers.value, (n) => n))
const filteredSshServers = computed(() => byName(sshServers.value, (s) => s.name))
const filteredWsAccounts = computed(() => byName(wsAccounts.value, (a) => a.name))
const credForm = ref({ name: '', service: 'telegram', fields: [{ key: 'access_token', value: '' }] })
const testingCred = ref(null)

const mcpModal = ref(false)
const sshModal = ref(false)
const wsModal = ref(false)

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

const collapsed = ref({
  credentials: true,
  mcp: true,
  ssh: true,
  ws: true,
  auth: true,
  messaging: true
})

async function load() {
  await Promise.all([loadMcp(), loadSsh(), loadWs(), loadAuth(), loadMessaging(), loadCredentials()])
}

async function loadCredentials() {
  const d = await get('/credentials')
  credentials.value = d.credentials || []
  credentialsSummary.value = credentials.value.length ? `${credentials.value.length} credentials` : 'none'
}

async function loadMcp() {
  const d = await get('/mcp')
  mcpServers.value = d.servers || []
  mcpTools.value = d.tools || []
  mcpSummary.value = mcpServers.value.length ? `${mcpServers.value.length} connected` : 'none'
}

async function loadSsh() {
  const d = await get('/ssh_servers')
  sshServers.value = d.servers || []
  sshSummary.value = sshServers.value.length
    ? `${sshServers.value.length} server${sshServers.value.length !== 1 ? 's' : ''}`
    : 'none'
}

async function loadWs() {
  const d = await get('/websearch/accounts')
  wsAccounts.value = d.accounts || []
  const enabled = wsAccounts.value.filter((a) => a.enabled).length
  wsSummary.value = wsAccounts.value.length ? `${enabled} active` : 'none'
}

async function loadAuth() {
  const d = await get('/integrations/status')
  authStatus.value = d.auth_status || {}
  const connected = ['google', 'microsoft', 'facebook', 'instagram'].filter(
    (p) => authStatus.value[p]?.authenticated
  )
  authSummary.value = connected.length
    ? `${connected.length}/4 connected`
    : 'none connected'
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
  
  const connectedCount = Object.values(messagingStatus.value).filter(s => s.connected).length
  messagingSummary.value = connectedCount ? `${connectedCount} active` : 'none'
}

async function saveMessagingToken(p) {
  const r = await put(`/settings/${encodeURIComponent(p.key)}`, { value: messagingTokens.value[p.id] })
  toast(r.ok ? `Saved ${p.name} token` : r.error, r.ok)
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
  <div class="services-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>Services</h1>
        <p class="page-desc">
          Manage your connections to external tools, databases, and authentication providers.
        </p>
      </div>
      <button
        class="btn btn-primary"
        @click="load"
      >
        <span style="margin-right:8px">↻</span> Refresh
      </button>
    </div>

    <SearchInput
      v-model="servicesSearch"
      placeholder="Search credentials, MCP servers, SSH servers, web search accounts…"
    />

    <!-- Credentials -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.credentials }"
    >
      <div
        class="card-header-row"
        @click="collapsed.credentials = !collapsed.credentials"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.credentials ? '▶' : '▼' }}</span>
          <h2>Credentials</h2>
        </div>
        <span class="card-summary">{{ credentialsSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="action-bar-modern">
          <button
            class="btn btn-premium-action"
            @click.stop="credModal = true"
          >
            <span class="plus-icon">+</span> Add Secure Credential
          </button>
        </div>
        
        <div class="service-list">
          <div
            v-for="c in filteredCredentials"
            :key="c.id"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="CATEGORY_META.credentials.icon"
                />
                <span class="service-name">{{ c.name }}</span>
                <span class="tag-pill">{{ c.service }}</span>
              </div>
              <div class="service-actions">
                <button
                  class="btn btn-sm btn-ghost"
                  :disabled="testingCred === c.id"
                  @click="testCredential(c.id)"
                >
                  {{ testingCred === c.id ? 'Testing…' : 'Test' }}
                </button>
                <button
                  class="btn btn-sm btn-danger"
                  @click="deleteCredential(c.id)"
                >
                  Delete
                </button>
              </div>
            </div>
          </div>
          <div
            v-if="filteredCredentials.length === 0"
            class="empty-state"
          >
            {{ servicesSearch.trim() ? 'No credentials match your search.' : 'No credentials configured.' }}
          </div>
        </div>
      </div>
    </div>

    <!-- MCP Servers -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.mcp }"
    >
      <div
        class="card-header-row"
        @click="collapsed.mcp = !collapsed.mcp"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.mcp ? '▶' : '▼' }}</span>
          <h2>MCP Servers</h2>
        </div>
        <span class="card-summary">{{ mcpSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="action-bar-modern">
          <button
            class="btn btn-premium-action"
            @click.stop="mcpModal = true"
          >
            <span class="plus-icon">+</span> Connect MCP Server
          </button>
        </div>
        
        <div class="service-list">
          <div
            v-for="name in filteredMcpServers"
            :key="name"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="CATEGORY_META.mcp.icon"
                />
                <span class="service-name">{{ name }}</span>
                <Pill
                  type="ok"
                  text="Connected"
                />
              </div>
              <div class="service-actions">
                <button
                  class="btn btn-sm btn-danger"
                  @click="disconnectMcp(name)"
                >
                  Disconnect
                </button>
              </div>
            </div>
            <div class="service-meta-line">
              {{ mcpTools.filter((t) => t.source?.server_name === name).length }} dynamic tools available
            </div>
          </div>
          <div
            v-if="filteredMcpServers.length === 0"
            class="empty-state"
          >
            {{ servicesSearch.trim() ? 'No MCP servers match your search.' : 'No MCP servers connected.' }}
          </div>
        </div>
      </div>
    </div>

    <!-- SSH Servers -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.ssh }"
    >
      <div
        class="card-header-row"
        @click="collapsed.ssh = !collapsed.ssh"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.ssh ? '▶' : '▼' }}</span>
          <h2>SSH Servers</h2>
        </div>
        <span class="card-summary">{{ sshSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="action-bar-modern">
          <button
            class="btn btn-premium-action"
            @click.stop="sshModal = true"
          >
            <span class="plus-icon">+</span> Register SSH Server
          </button>
        </div>
        
        <div class="service-list">
          <div
            v-for="s in filteredSshServers"
            :key="s.name"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="CATEGORY_META.ssh.icon"
                />
                <span class="service-name">{{ s.name }}</span>
                <Pill
                  type="ok"
                  text="Configured"
                />
              </div>
              <div class="service-actions">
                <button
                  class="btn btn-sm btn-danger"
                  @click="deleteSsh(s.name)"
                >
                  Remove
                </button>
              </div>
            </div>
            <div class="service-meta-line">
              {{ s.username }}@{{ s.ip }}:{{ s.port }} · {{ s.auth_type }}
            </div>
          </div>
          <div
            v-if="filteredSshServers.length === 0"
            class="empty-state"
          >
            {{ servicesSearch.trim() ? 'No SSH servers match your search.' : 'No SSH servers configured.' }}
          </div>
        </div>
      </div>
    </div>

    <!-- Web Search / Tavily -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.ws }"
    >
      <div
        class="card-header-row"
        @click="collapsed.ws = !collapsed.ws"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.ws ? '▶' : '▼' }}</span>
          <h2>Web Search</h2>
        </div>
        <span class="card-summary">{{ wsSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="action-bar-modern gap-12">
          <button
            class="btn btn-premium-action"
            @click.stop="showAddWs"
          >
            <span class="plus-icon">+</span> Add Tavily Account
          </button>
          <button
            class="btn btn-ghost"
            style="border-radius:12px;"
            @click.stop="resetWsQuotas"
          >
            Reset Quotas
          </button>
        </div>
        
        <div class="service-list">
          <div
            v-for="a in filteredWsAccounts"
            :key="a.id"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="CATEGORY_META.ws.icon"
                />
                <span class="service-name">{{ a.name }}</span>
                <Pill
                  :type="a.enabled ? 'ok' : 'err'"
                  :text="a.enabled ? 'Enabled' : 'Exhausted'"
                />
              </div>
              <div class="service-actions">
                <button
                  class="btn btn-sm btn-ghost"
                  @click="showEditWs(a)"
                >
                  Edit
                </button>
                <button
                  class="btn btn-sm btn-danger"
                  @click="deleteWs(a.id)"
                >
                  Delete
                </button>
              </div>
            </div>
            <div class="service-meta-line">
              Priority {{ a.priority }} · {{ a.queries_this_month }} queries this cycle
            </div>
          </div>
          <div
            v-if="filteredWsAccounts.length === 0"
            class="empty-state"
          >
            {{ servicesSearch.trim() ? 'No web search accounts match your search.' : 'No Tavily accounts configured.' }}
          </div>
        </div>
      </div>
    </div>

    <!-- Messaging Platforms -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.messaging }"
    >
      <div
        class="card-header-row"
        @click="collapsed.messaging = !collapsed.messaging"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.messaging ? '▶' : '▼' }}</span>
          <h2>Messaging Platforms</h2>
        </div>
        <span class="card-summary">{{ messagingSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="service-list">
          <div
            v-for="p in PLATFORMS"
            :key="p.id"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="p.icon"
                />
                <span class="service-name">{{ p.name }}</span>
                <Pill
                  :type="isMessagingConnected(p) ? 'ok' : (isMessagingConfigured(p) ? 'info' : 'muted')"
                  :text="isMessagingConnected(p) ? 'Active' : (isMessagingConfigured(p) ? 'Ready' : 'Offline')"
                />
              </div>
              <div class="service-actions">
                <button
                  class="btn btn-sm btn-ghost"
                  :disabled="!isMessagingConfigured(p)"
                  @click="reconnectMessaging(p)"
                >
                  {{ isMessagingConnected(p) ? 'Restart' : 'Connect' }}
                </button>
              </div>
            </div>
            <div class="service-meta-line">
              {{ p.desc }}
            </div>
            <div class="inline-token-row">
              <input
                v-model="messagingTokens[p.id]"
                type="password"
                class="token-input-compact"
                placeholder="Paste bot token…"
              >
              <button
                class="btn btn-sm btn-ghost"
                @click="saveMessagingToken(p)"
              >
                Save
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- Authentication -->
    <div
      class="premium-card collapsible"
      :class="{ collapsed: collapsed.auth }"
    >
      <div
        class="card-header-row"
        @click="collapsed.auth = !collapsed.auth"
      >
        <div class="card-title-group">
          <span class="collapse-icon">{{ collapsed.auth ? '▶' : '▼' }}</span>
          <h2>Authentication</h2>
        </div>
        <span class="card-summary">{{ authSummary }}</span>
      </div>
      
      <div class="card-content">
        <div class="service-list">
          <div
            v-for="(meta, id) in AUTH_METADATA"
            :key="id"
            class="service-item"
          >
            <div class="service-name-row">
              <div class="service-name-group">
                <span
                  class="service-icon-sm"
                  v-html="meta.icon"
                />
                <span class="service-name">{{ meta.name }}</span>
                <Pill
                  :type="authStatus[id]?.authenticated ? 'ok' : 'muted'"
                  :text="authStatus[id]?.authenticated ? 'Connected' : 'Not connected'"
                />
              </div>
              <div class="service-actions">
                <button
                  v-if="!authStatus[id]?.authenticated"
                  class="btn btn-sm btn-ghost"
                  @click="connectAuth(id)"
                >
                  Connect
                </button>
                <button
                  v-else
                  class="btn btn-sm btn-danger"
                  @click="disconnectAuth(id)"
                >
                  Disconnect
                </button>
              </div>
            </div>
            <div class="service-meta-line">
              <template v-if="authStatus[id]?.authenticated">
                {{ authStatus[id]?.user || authStatus[id]?.email || 'Session active' }}
              </template>
              <template v-else>
                Secure {{ meta.name }} integration
              </template>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- MODALS -->

  <!-- MCP Modal -->
  <Modal
    v-model="mcpModal"
    title="Connect MCP Server"
  >
    <div class="form-container">
      <div class="form-group-modern">
        <label>Name</label>
        <input
          v-model="mcpForm.name"
          type="text"
          class="premium-input"
          placeholder="e.g. Local Tools"
        >
      </div>
      <div class="form-group-modern">
        <label>URL</label>
        <input
          v-model="mcpForm.url"
          type="text"
          class="premium-input"
          placeholder="http://localhost:8000"
        >
      </div>
      <div class="form-group-modern">
        <label>API Key / Token (Optional)</label>
        <input
          v-model="mcpForm.api_key"
          type="password"
          class="premium-input"
          placeholder="••••••••••••••••"
        >
      </div>
    </div>
    <div class="modal-actions-modern">
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
        Connect Server
      </button>
    </div>
  </Modal>

  <!-- SSH Modal -->
  <Modal
    v-model="sshModal"
    title="Add SSH Server"
  >
    <div class="form-container">
      <div class="form-group-modern">
        <label>Server Name</label>
        <input
          v-model="sshForm.name"
          type="text"
          class="premium-input"
          placeholder="e.g. prod-db-1"
        >
      </div>
      
      <div class="form-row-modern">
        <div class="form-group-modern flex-2">
          <label>IP / Hostname</label>
          <input
            v-model="sshForm.ip"
            type="text"
            class="premium-input"
            placeholder="192.168.1.1"
          >
        </div>
        <div class="form-group-modern flex-1">
          <label>Port</label>
          <input
            v-model="sshForm.port"
            type="number"
            class="premium-input"
          >
        </div>
      </div>
      
      <div class="form-group-modern">
        <label>Username</label>
        <input
          v-model="sshForm.username"
          type="text"
          class="premium-input"
          placeholder="root"
        >
      </div>
      
      <div class="form-group-modern">
        <label>Auth Type</label>
        <select
          v-model="sshForm.auth_type"
          class="premium-input select-input"
        >
          <option value="key">
            SSH Key Pair
          </option>
          <option value="password">
            Password
          </option>
        </select>
      </div>
      
      <div
        v-if="sshForm.auth_type === 'key'"
        class="auth-details"
      >
        <div class="form-group-modern">
          <label>Private Key (PEM/OpenSSH)</label>
          <textarea
            v-model="sshForm.private_key"
            class="premium-input textarea-input code-font"
            rows="4"
            placeholder="-----BEGIN OPENSSH PRIVATE KEY..."
          />
        </div>
        <div class="form-group-modern">
          <label>Public Key (Optional)</label>
          <textarea
            v-model="sshForm.public_key"
            class="premium-input textarea-input code-font"
            rows="2"
          />
        </div>
      </div>
      
      <div
        v-else
        class="form-group-modern auth-details"
      >
        <label>Password</label>
        <input
          v-model="sshForm.password"
          type="password"
          class="premium-input"
          placeholder="••••••••••••••••"
        >
      </div>
    </div>
    
    <div class="modal-actions-modern">
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
  </Modal>

  <!-- Web Search Modal -->
  <Modal
    v-model="wsModal"
    :title="wsEditing ? 'Edit Account' : 'Add Tavily Account'"
  >
    <div class="form-container">
      <div class="form-group-modern">
        <label>Account Name</label>
        <input
          v-model="wsForm.name"
          type="text"
          class="premium-input"
          placeholder="e.g. Tavily Personal"
        >
      </div>
      <div class="form-group-modern">
        <label>Tavily API Key</label>
        <input
          v-model="wsForm.api_key"
          type="password"
          class="premium-input"
          placeholder="tvly-..."
        >
      </div>
      <div class="form-row-modern align-center">
        <div class="form-group-modern flex-1 mb-0">
          <label>Priority</label>
          <input
            v-model="wsForm.priority"
            type="number"
            min="1"
            class="premium-input"
          >
        </div>
        <div class="form-group-modern flex-1 mb-0 check-group">
          <label class="checkbox-label">
            <span class="custom-checkbox">
              <input
                v-model="wsForm.enabled"
                type="checkbox"
              >
              <span class="checkmark" />
            </span>
            Enabled
          </label>
        </div>
      </div>
    </div>
    <div class="modal-actions-modern">
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
  </Modal>

  <!-- Credential Modal -->
  <Modal
    v-model="credModal"
    title="Add Credential"
  >
    <div class="form-container">
      <div class="form-group-modern">
        <label>Name</label>
        <input
          v-model="credForm.name"
          type="text"
          class="premium-input"
          placeholder="e.g. My Prod Bot Token"
        >
      </div>
      <div class="form-group-modern">
        <label>Service (Platform)</label>
        <input
          v-model="credForm.service"
          type="text"
          class="premium-input"
          placeholder="e.g. telegram"
        >
      </div>
      <div class="form-group-modern">
        <label>Fields</label>
        <div
          v-for="(field, index) in credForm.fields"
          :key="index"
          style="display:flex;gap:10px;margin-bottom:8px;align-items:center;"
        >
          <input
            v-model="field.key"
            type="text"
            class="premium-input"
            placeholder="Key (e.g. access_token)"
            style="flex:1"
          >
          <!-- password, not text: these fields hold credential secrets
               (access_token, api_key, private_key, ...) and were previously
               rendered in plaintext while being typed. -->
          <input
            v-model="field.value"
            type="password"
            class="premium-input"
            placeholder="Value"
            style="flex:2"
          >
          <button
            class="btn btn-sm btn-ghost"
            style="flex-shrink:0;"
            @click="credForm.fields.splice(index, 1)"
          >
            ✕
          </button>
        </div>
        <button
          class="btn btn-sm btn-ghost"
          style="width:100%"
          @click="credForm.fields.push({ key: '', value: '' })"
        >
          + Add Field
        </button>
      </div>
    </div>
    <div class="modal-actions-modern">
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
  </Modal>
</template>

<style scoped>
.services-page {
  padding-bottom: 60px;
}

.page-header-container {
  display: flex;
  justify-content: space-between;
  align-items: flex-end;
  margin-bottom: 30px;
}

.page-header h1 {
  font-size: 28px;
  font-weight: 800;
  letter-spacing: -0.02em;
  margin-bottom: 8px;
  background: linear-gradient(90deg, #1e2433, #6c5ce7);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.page-desc {
  color: var(--muted);
  font-size: 14px;
  margin: 0;
}

/* Premium Cards */
.premium-card {
  background: rgba(255, 255, 255, 0.4);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(0, 0, 0, 0.05);
  border-radius: 16px;
  box-shadow: 0 10px 40px rgba(0, 0, 0, 0.2);
  margin-bottom: 24px;
  overflow: hidden;
  transition: all 0.3s cubic-bezier(0.16, 1, 0.3, 1);
}

.premium-card:hover {
  box-shadow: 0 15px 50px rgba(0, 0, 0, 0.3);
  border-color: rgba(0, 0, 0, 0.08);
}

.card-header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px 24px;
  background: rgba(0, 0, 0, 0.1);
  cursor: pointer;
  transition: background 0.2s;
}

.card-header-row:hover {
  background: rgba(0, 0, 0, 0.02);
}

.card-title-group {
  display: flex;
  align-items: center;
  gap: 16px;
}

.collapse-icon {
  font-size: 12px;
  color: var(--muted);
  width: 16px;
  display: inline-block;
  text-align: center;
  transition: transform 0.2s;
}

.card-header-row h2 {
  font-size: 13px;
  font-weight: 800;
  color: var(--text);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  margin: 0;
}

.card-summary {
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
  background: rgba(0, 0, 0, 0.2);
  padding: 4px 10px;
  border-radius: 20px;
}

.card-content {
  border-top: 1px solid rgba(0, 0, 0, 0.03);
  display: block;
  overflow: hidden;
}

.premium-card.collapsible.collapsed .card-content {
  display: none;
}

/* Service Lists */
.action-bar-modern {
  padding: 14px 24px;
  background: rgba(0, 0, 0, 0.01);
  display: flex;
  justify-content: flex-end;
  border-bottom: 1px solid rgba(0, 0, 0, 0.03);
}

.gap-12 { gap: 12px; }

.btn-premium-action {
  background: rgba(0, 0, 0, 0.05);
  border: 1px solid rgba(0, 0, 0, 0.1);
  color: var(--text);
  font-weight: 700;
  padding: 8px 16px;
  border-radius: 10px;
  display: flex;
  align-items: center;
  gap: 10px;
  transition: all 0.2s;
  cursor: pointer;
  font-size: 13px;
}

.btn-premium-action:hover {
  background: #fff;
  color: #000;
  transform: translateY(-2px);
}

.plus-icon {
  font-size: 16px;
  font-weight: 400;
  line-height: 1;
}

.empty-state {
  padding: 30px;
  text-align: center;
  color: var(--muted);
  font-size: 14px;
  font-style: italic;
  width: 100%;
}

.service-list {
  display: flex;
  flex-direction: column;
}

.service-item {
  padding: 16px 24px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.05);
  transition: background 0.2s;
}

.service-item:last-child {
  border-bottom: none;
}

.service-item:hover {
  background: rgba(0, 0, 0, 0.02);
}

.service-name-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.service-name-group {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.service-icon-sm {
  width: 22px;
  height: 22px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.2);
  border-radius: 6px;
  padding: 4px;
}

.service-icon-sm svg {
  width: 100%;
  height: 100%;
}

.service-name {
  font-size: 14px;
  font-weight: 700;
  color: var(--text);
  white-space: nowrap;
}

.tag-pill {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--muted);
  background: rgba(0, 0, 0, 0.2);
  padding: 2px 8px;
  border-radius: 4px;
}

.service-actions {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
}

.service-meta-line {
  margin-top: 6px;
  font-size: 12px;
  color: var(--muted);
  line-height: 1.5;
}

.inline-token-row {
  display: flex;
  gap: 8px;
  margin-top: 10px;
  max-width: 420px;
}

.token-input-compact {
  flex: 1;
  background: rgba(0, 0, 0, 0.2);
  border: 1px solid rgba(0, 0, 0, 0.05);
  border-radius: 8px;
  color: var(--text);
  padding: 6px 10px;
  font-size: 12px;
  font-family: 'Fira Code', monospace;
  outline: none;
  margin-bottom: 0;
}

.token-input-compact:focus {
  border-color: var(--teal);
}

/* Custom Checkbox */
.check-group {
  font-weight: 600;
  letter-spacing: 0.02em;
  border-radius: 8px;
  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
  padding: 10px 18px;
  font-size: 13px;
}

.btn-sm {
  padding: 6px 12px;
  font-size: 12px;
}

.btn-save {
  background: linear-gradient(135deg, #00b894 0%, #00cec9 100%);
  color: #fff;
  border: none;
  font-weight: 700;
  box-shadow: 0 4px 15px rgba(0, 206, 201, 0.2);
}

.btn-save:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 25px rgba(0, 206, 201, 0.3);
}

.btn-save:active {
  transform: translateY(0);
}

.btn-primary {
  background: linear-gradient(135deg, #6c5ce7 0%, #a29bfe 100%);
  color: #fff;
  border: none;
  box-shadow: 0 4px 15px rgba(108, 92, 231, 0.2);
}

.btn-primary:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 25px rgba(108, 92, 231, 0.3);
}

.btn-primary:disabled {
  background: rgba(0, 0, 0, 0.05);
  color: var(--muted);
  box-shadow: none;
  transform: none;
  cursor: not-allowed;
}

.btn-ghost {
  background: rgba(0, 0, 0, 0.05);
  border: 1px solid rgba(0, 0, 0, 0.1);
  color: var(--text);
}

.btn-ghost:hover {
  background: rgba(0, 0, 0, 0.1);
  border-color: rgba(0, 0, 0, 0.2);
}

.btn-danger {
  background: rgba(244, 63, 94, 0.1);
  color: #fb7185;
  border: 1px solid rgba(244, 63, 94, 0.2);
}

.btn-danger:hover {
  background: rgba(244, 63, 94, 0.2);
  border-color: rgba(244, 63, 94, 0.4);
  color: var(--text);
}

/* Modal Modernization */
.form-container {
  display: flex;
  flex-direction: column;
  gap: 20px;
  margin-bottom: 24px;
}

.form-group-modern {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.form-group-modern label {
  font-size: 12px;
  font-weight: 700;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.form-row-modern {
  display: flex;
  gap: 16px;
}

.align-center {
  align-items: center;
}

.flex-1 { flex: 1; }
.flex-2 { flex: 2; }
.mb-0 { margin-bottom: 0; }

.auth-details {
  animation: slide-down 0.3s cubic-bezier(0.16, 1, 0.3, 1);
  background: rgba(0, 0, 0, 0.2);
  padding: 16px;
  border-radius: 12px;
  border: 1px solid rgba(0, 0, 0, 0.03);
  display: flex;
  flex-direction: column;
  gap: 16px;
}

@keyframes slide-down {
  from { opacity: 0; transform: translateY(-10px); }
  to { opacity: 1; transform: translateY(0); }
}

.premium-input {
  width: 100%;
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(0, 0, 0, 0.08);
  border-radius: 10px;
  color: var(--text);
  padding: 12px 16px;
  font-size: 14px;
  font-family: inherit;
  transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
  outline: none;
}

.premium-input::placeholder {
  color: rgba(0, 0, 0, 0.2);
}

.premium-input:focus {
  background: rgba(0, 0, 0, 0.4);
  border-color: #2c9b8d;
  box-shadow: 0 0 0 3px rgba(129, 230, 217, 0.1);
}

.select-input {
  appearance: none;
  background-image: url("data:image/svg+xml;charset=UTF-8,%3csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='white' stroke-width='2' stroke-linecap='round' stroke-linejoin='round' opacity='0.5'%3e%3cpolyline points='6 9 12 15 18 9'%3e%3c/polyline%3e%3c/svg%3e");
  background-repeat: no-repeat;
  background-position: right 12px center;
  background-size: 16px;
  padding-right: 40px;
}

.textarea-input {
  resize: vertical;
  line-height: 1.5;
}

.code-font {
  font-family: 'Fira Code', monospace;
  font-size: 12px;
  color: #a29bfe;
}

.modal-actions-modern {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  padding-top: 20px;
  border-top: 1px solid rgba(0, 0, 0, 0.05);
}

/* Custom Checkbox */
.check-group {
  margin-top: 24px;
}

.checkbox-label {
  display: flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  font-size: 14px !important;
  text-transform: none !important;
  font-weight: 500 !important;
  color: var(--text) !important;
  letter-spacing: 0 !important;
}

.custom-checkbox {
  position: relative;
  display: inline-block;
  width: 20px;
  height: 20px;
}

.custom-checkbox input {
  opacity: 0;
  width: 0;
  height: 0;
}

.checkmark {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background-color: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(0, 0, 0, 0.15);
  border-radius: 6px;
  transition: all 0.2s;
}

.custom-checkbox input:checked ~ .checkmark {
  background-color: #00b894;
  border-color: #00b894;
}

.checkmark:after {
  content: "";
  position: absolute;
  display: none;
  left: 6px;
  top: 2px;
  width: 6px;
  height: 10px;
  border: solid white;
  border-width: 0 2px 2px 0;
  transform: rotate(45deg);
}

.custom-checkbox input:checked ~ .checkmark:after {
  display: block;
}
</style>

