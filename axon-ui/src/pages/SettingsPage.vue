<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import Pill from '../components/Pill.vue'
import SearchInput from '../components/SearchInput.vue'

const byCategory = ref({})
const settingsSearch = ref('')
const patternsText = ref('[\n]')
const testMsg = ref('')
const testResult = ref(null)
const activeSection = ref('')
const loaded = ref(false)
const retentionRunning = ref(false)
const retentionResult = ref('')

const CATEGORY_META = {
  agent: { title: 'Agent', description: 'Core agent-loop behavior: iteration and correction budgets, run/tool timeouts, tool scope, reasoning effort, temperature, token caps, and the system prompt.' },
  backup: { title: 'Backups', description: 'Daily local snapshots of axon.db and crm.db (VACUUM INTO), written to the Files page directory and pruned after the configured retention. These are on-instance only — copying them off the server is the operator’s responsibility.' },
  crm: { title: 'CRM', description: 'CRM behavior. Chat-agent write access (create/update/delete/convert/archive) is gated per-tool on the Tools page, not here — reads are always available to the agent.' },
  embedder: { title: 'Embeddings', description: 'OpenAI-compatible embeddings provider powering the semantic tool-routing tier and long-term memory recall.' },
  instagram: { title: 'Instagram Publishing', description: 'Media hosting URLs, bind address, TTL, and image/video processing waits.' },
  memory: { title: 'Memory', description: 'Retention, recall, and knowledge persistence behavior.' },
  messaging: { title: 'Messaging', description: 'Chat gateway tokens (Telegram, Discord, Slack) and Telegram workflow-runner access control.' },
  retention: { title: 'Database Retention', description: 'How long agent run history, tool observations, workflow runs, and webhook events are kept before the daily housekeeping sweep prunes them. Lower values keep the database smaller.' },
  router: { title: 'Router', description: 'Model failover behavior and the pattern → embedding → LLM tool-routing tiers.' },
  scheduler: { title: 'Scheduler', description: 'Background jobs, polling cadence, and automation timing.' },
  watcher: { title: 'Smart Notifications', description: 'Auto-polling watchers (Gmail, Outlook, Calendar, Facebook), quiet hours, and where notifications are delivered.' },
  websearch: { title: 'Web Search', description: 'Search provider behavior and retrieval policy.' },
  workflow: { title: 'Workflows', description: 'Run concurrency and queueing, version snapshots, resume/approval links, and webhook deduplication.' },
}

function humanizeCategory(cat) {
  return String(cat || 'other')
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase())
}

const categoryEntries = computed(() =>
  Object.entries(byCategory.value)
    .map(([key, rows]) => ({
      key,
      rows,
      meta: CATEGORY_META[key] || {
        title: humanizeCategory(key),
        description: `${humanizeCategory(key)} configuration for the dashboard runtime.`,
      },
    }))
    .sort((a, b) => a.meta.title.localeCompare(b.meta.title))
)

const sections = computed(() => [
  ...categoryEntries.value.map((category) => ({
    id: `category:${category.key}`,
    title: category.meta.title,
    meta: `${category.rows.length} settings`,
  })),
  { id: 'router:patterns', title: 'Router Patterns', meta: 'JSON rules' },
  { id: 'router:test', title: 'Router Test', meta: `${routerMatchCount.value} matches` },
])

const activeCategory = computed(() => {
  if (!activeSection.value.startsWith('category:')) return null
  const key = activeSection.value.slice('category:'.length)
  return categoryEntries.value.find((item) => item.key === key) || null
})

const showingPatterns = computed(() => activeSection.value === 'router:patterns')
const showingRouterTest = computed(() => activeSection.value === 'router:test')

const routerMatchCount = computed(() => testResult.value?.matched_tools?.length || 0)

watch(
  sections,
  (next) => {
    if (!next.length) {
      activeSection.value = ''
      return
    }
    // Only default once the real settings have loaded. Otherwise, on mount the
    // categories are still empty and `sections` only contains the router
    // entries, which would incorrectly default the selection to Router Patterns.
    if (!loaded.value) return
    if (!next.some((item) => item.id === activeSection.value)) {
      activeSection.value = next[0].id
    }
  },
  { immediate: true }
)

async function load() {
  const [d, pData] = await Promise.all([get('/settings'), get('/patterns')])
  const settings = d.settings || []
  const grouped = {}
  settings.forEach((s) => {
    if (s.category === 'providers') return
    const cat = s.category || 'other'
    if (!grouped[cat]) grouped[cat] = []
    grouped[cat].push({ ...s, draft: s.value })
  })
  byCategory.value = grouped

  const pats = pData.patterns || []
  patternsText.value = JSON.stringify(
    pats.map((p) => ({
      tool_name: p.tool_name,
      pattern: p.pattern,
      description: p.description || '',
      enabled: p.enabled,
    })),
    null,
    2
  )

  loaded.value = true
}

async function savePatterns() {
  let parsed
  try {
    parsed = JSON.parse(patternsText.value)
  } catch (_e) {
    return toast('Invalid JSON format', false)
  }
  const r = await put('/patterns/bulk', { patterns: parsed })
  toast(r.ok ? 'Patterns saved' : r.error, r.ok)
  if (r.ok) load()
}

async function testRouter() {
  if (!testMsg.value) return
  testResult.value = await post('/patterns/test', { message: testMsg.value })
}

async function saveCategory(cat) {
  const rows = byCategory.value[cat]
  let saved = 0
  const errors = []
  for (const s of rows) {
    if (s.draft === s.value) continue
    const r = await put(`/settings/${encodeURIComponent(s.key)}`, {
      value: s.draft,
    })
    if (r.ok) {
      s.value = s.draft
      saved += 1
    } else {
      errors.push(`${s.key}: ${r.error}`)
    }
  }

  if (errors.length) toast(`Errors: ${errors.join(', ')}`, false)
  else if (saved > 0) toast(`Saved ${saved} setting${saved > 1 ? 's' : ''}`, true)
  else toast('No changes to save', true)
}

async function runRetentionNow() {
  if (retentionRunning.value) return
  retentionRunning.value = true
  retentionResult.value = ''
  const r = await post('/retention/run')
  retentionRunning.value = false
  if (r.ok) {
    retentionResult.value = r.summary || 'Cleanup complete'
    toast('Cleanup complete', true)
  } else {
    toast(r.error || 'Cleanup failed', false)
  }
}

function selectSection(id) {
  activeSection.value = id
}

function isSecret(s) {
  // Only string values can be secrets — int knobs like max_total_tokens or
  // resume_token_default_ttl_secs must not be masked just for containing "token".
  if (s.value_type !== 'string') return false
  const k = s.key.toLowerCase()
  return k.includes('key') || k.includes('token') || k.includes('password')
}

function isPrompt(key) {
  const k = key.toLowerCase()
  return k.includes('prompt') || k.includes('instruction')
}

onMounted(load)
</script>

<template>
  <div class="settings-page services-page">
    <div class="page-section-layout">
      <aside class="page-section-sidebar">
        <nav class="page-section-nav">
          <button
            v-for="section in sections"
            :key="section.id"
            type="button"
            class="page-section-nav-btn"
            :class="{ active: section.id === activeSection }"
            @click="selectSection(section.id)"
          >
            <span class="page-section-nav-title">{{ section.title }}</span>
            <span class="page-section-nav-meta">{{ section.meta }}</span>
          </button>
        </nav>
      </aside>

      <div class="page-section-content">
        <section
          v-if="activeCategory"
          class="settings-card premium-card"
        >
          <div class="settings-card-header">
            <div>
              <span class="settings-section-kicker">{{ activeCategory.key }}</span>
              <h2>{{ activeCategory.meta.title }}</h2>
              <p class="section-desc">
                {{ activeCategory.meta.description }}
              </p>
            </div>
            <span class="card-summary">{{ activeCategory.rows.length }} settings</span>
          </div>

          <div class="settings-list">
            <div
              v-for="s in activeCategory.rows"
              :key="s.key"
              class="setting-item"
            >
              <div class="setting-copy">
                <div class="setting-title-row">
                  <span class="setting-key">{{ s.key }}</span>
                  <Pill
                    type="muted"
                    :text="s.value_type"
                  />
                </div>
                <p
                  v-if="s.description"
                  class="setting-desc"
                >
                  {{ s.description }}
                </p>
              </div>

              <div class="setting-control">
                <textarea
                  v-if="isPrompt(s.key)"
                  v-model="s.draft"
                  class="premium-input setting-input setting-input-lg"
                  spellcheck="false"
                  placeholder="Enter prompt instructions"
                />

                <input
                  v-else-if="isSecret(s)"
                  v-model="s.draft"
                  type="password"
                  class="premium-input setting-input"
                  placeholder="Hidden value"
                >

                <label
                  v-else-if="s.value_type === 'bool'"
                  class="setting-toggle"
                >
                  <input
                    type="checkbox"
                    class="setting-toggle-input"
                    :checked="s.draft === 'true'"
                    @change="s.draft = $event.target.checked ? 'true' : 'false'"
                  >
                  <span class="setting-toggle-track"><span class="setting-toggle-thumb" /></span>
                  <span class="setting-toggle-text">{{ s.draft === 'true' ? 'On' : 'Off' }}</span>
                </label>

                <input
                  v-else-if="s.value_type === 'int'"
                  v-model="s.draft"
                  type="number"
                  step="1"
                  class="premium-input setting-input"
                  placeholder="0"
                >

                <input
                  v-else
                  v-model="s.draft"
                  type="text"
                  class="premium-input setting-input"
                  placeholder="Value"
                >
              </div>
            </div>
          </div>

          <div class="settings-card-footer">
            <button
              class="btn btn-save"
              @click="saveCategory(activeCategory.key)"
            >
              Save {{ activeCategory.meta.title }}
            </button>
            <div
              v-if="activeCategory.key === 'retention'"
              class="retention-actions"
            >
              <span
                v-if="retentionResult"
                class="retention-result"
              >{{ retentionResult }}</span>
              <button
                class="btn"
                :disabled="retentionRunning"
                @click="runRetentionNow"
              >
                {{ retentionRunning ? 'Running…' : 'Run cleanup now' }}
              </button>
            </div>
          </div>
        </section>

        <section
          v-else-if="showingPatterns"
          class="settings-card premium-card"
        >
          <div class="settings-card-header">
            <div>
              <span class="settings-section-kicker">router</span>
              <h2>Tool Router Patterns</h2>
              <p class="section-desc">
                Maintain pattern rules as JSON. Each rule should include `tool_name`, `pattern`, and optional metadata.
              </p>
            </div>
            <span class="card-summary">JSON</span>
          </div>

          <div class="editor-shell">
            <div class="editor-shell-header">
              Pattern Rules
            </div>
            <textarea
              v-model="patternsText"
              class="premium-input code-editor"
              spellcheck="false"
            />
          </div>

          <div class="settings-card-footer">
            <button
              class="btn btn-save"
              @click="savePatterns"
            >
              Save Patterns
            </button>
          </div>
        </section>

        <section
          v-else-if="showingRouterTest"
          class="settings-card premium-card"
        >
          <div class="settings-card-header">
            <div>
              <span class="settings-section-kicker">router</span>
              <h2>Live Router Test</h2>
              <p class="section-desc">
                Validate how a message is categorized before it reaches the agent runtime.
              </p>
            </div>
            <span class="card-summary">{{ routerMatchCount }} matches</span>
          </div>

          <div class="router-test-shell">
            <div class="router-test-input">
              <input
                v-model="testMsg"
                type="text"
                class="premium-input setting-input"
                placeholder="Type a message to test routing"
                @keydown.enter="testRouter"
              >
              <button
                class="btn btn-primary"
                @click="testRouter"
              >
                Run Test
              </button>
            </div>

            <div
              class="router-test-result"
              :class="{ populated: testResult }"
            >
              <template v-if="testResult">
                <div class="router-result-row">
                  <span class="router-result-label">Tier</span>
                  <Pill
                    type="info"
                    :text="testResult.routing_info?.tier || '?'"
                  />
                </div>

                <div class="router-result-row router-result-tools">
                  <span class="router-result-label">Matched Tools</span>
                  <div class="matched-tools-list">
                    <template v-if="testResult.matched_tools?.length">
                      <Pill
                        v-for="t in testResult.matched_tools"
                        :key="t"
                        type="ok"
                        :text="t"
                      />
                    </template>
                    <Pill
                      v-else
                      type="muted"
                      text="None"
                    />
                  </div>
                </div>
              </template>

              <div
                v-else
                class="router-placeholder"
              >
                Run a message through the router to inspect its tier and matching tools.
              </div>
            </div>
          </div>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
.settings-page {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.settings-section-kicker {
  display: inline-block;
  margin-bottom: 10px;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--muted);
}

.settings-card-header,
.settings-card-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 20px 22px;
}

.settings-card-header {
  border-bottom: 1px solid rgba(0, 0, 0, 0.05);
}

.settings-card-header h2 {
  margin: 0;
  font-size: 20px;
  font-weight: 700;
}

.section-desc {
  margin: 8px 0 0;
  max-width: 720px;
  color: var(--muted);
  line-height: 1.6;
}

.settings-card-footer {
  border-top: 1px solid rgba(0, 0, 0, 0.05);
  justify-content: flex-end;
}

.settings-list {
  display: flex;
  flex-direction: column;
  gap: 0;
  padding: 10px;
}

.setting-item {
  display: grid;
  grid-template-columns: minmax(260px, 0.95fr) minmax(0, 1.3fr);
  gap: 18px;
  align-items: start;
  padding: 16px;
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.02);
}

.setting-item + .setting-item {
  margin-top: 10px;
}

.setting-title-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
  margin-bottom: 8px;
}

.setting-key {
  font-size: 15px;
  font-weight: 700;
}

.setting-desc {
  margin: 0;
  color: var(--muted);
  line-height: 1.55;
}

.setting-input {
  width: 100%;
}

.setting-input-lg {
  min-height: 150px;
  resize: vertical;
  font-family: 'Consolas', 'SFMono-Regular', monospace;
}

.setting-toggle {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  cursor: pointer;
  user-select: none;
}

.setting-toggle-input {
  position: absolute;
  opacity: 0;
  width: 0;
  height: 0;
}

.setting-toggle-track {
  position: relative;
  width: 42px;
  height: 24px;
  border-radius: 999px;
  background: var(--muted);
  transition: background 0.15s ease;
  flex-shrink: 0;
}

.setting-toggle-thumb {
  position: absolute;
  top: 3px;
  left: 3px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  transition: transform 0.15s ease;
}

.setting-toggle-input:checked + .setting-toggle-track {
  background: var(--accent);
}

.setting-toggle-input:checked + .setting-toggle-track .setting-toggle-thumb {
  transform: translateX(18px);
}

.setting-toggle-input:focus-visible + .setting-toggle-track {
  box-shadow: 0 0 0 3px var(--accentDim);
}

.setting-toggle-text {
  font-size: 14px;
  font-weight: 600;
  color: var(--muted);
}

.retention-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-left: auto;
}

.retention-result {
  font-size: 12.5px;
  color: var(--muted);
  text-align: right;
  max-width: 380px;
  line-height: 1.4;
}

.editor-shell,
.router-test-shell {
  padding: 20px 22px 22px;
}

.editor-shell-header {
  padding: 12px 14px;
  border: 1px solid rgba(0, 0, 0, 0.06);
  border-bottom: 0;
  border-radius: 8px 8px 0 0;
  background: rgba(0, 0, 0, 0.02);
  font-size: 12px;
  font-weight: 700;
  color: var(--muted);
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.code-editor {
  min-height: 340px;
  border-radius: 0 0 8px 8px !important;
  font-family: 'Consolas', 'SFMono-Regular', monospace;
  line-height: 1.65;
  resize: vertical;
}

.router-test-shell {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.router-test-input {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px;
}

.router-test-result {
  min-height: 180px;
  padding: 18px;
  border: 1px solid rgba(0, 0, 0, 0.06);
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.02);
}

.router-test-result.populated {
  background: rgba(37, 194, 209, 0.04);
  border-color: rgba(37, 194, 209, 0.14);
}

.router-result-row {
  display: flex;
  align-items: flex-start;
  gap: 14px;
}

.router-result-row + .router-result-row {
  margin-top: 16px;
}

.router-result-label {
  min-width: 96px;
  padding-top: 2px;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}

.matched-tools-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.router-placeholder {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  text-align: center;
  color: var(--muted);
  line-height: 1.6;
}

@media (max-width: 960px) {
  .setting-item {
    grid-template-columns: 1fr;
  }

  .router-test-input {
    grid-template-columns: 1fr;
  }
}
</style>
