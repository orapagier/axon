<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { useHeaderSearch } from '../lib/headerSearch.js'

const byCategory = ref({})
const settingsSearch = ref('')
const patternsText = ref('[\n]')
const patternsOriginal = ref('[\n]')
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
    meta: String(category.rows.length),
  })),
  { id: 'router:patterns', title: 'Router Patterns', meta: 'json', divider: true },
  { id: 'router:test', title: 'Router Test', meta: String(routerMatchCount.value) },
])

const activeCategory = computed(() => {
  if (!activeSection.value.startsWith('category:')) return null
  const key = activeSection.value.slice('category:'.length)
  return categoryEntries.value.find((item) => item.key === key) || null
})

const searchActive = computed(() => settingsSearch.value.trim().length > 0)

// A query searches EVERY category, not just the selected one: matching rows
// are grouped per category so each keeps its own Save button. Row objects are
// the same references as byCategory, so drafts edited here persist.
const searchResults = computed(() => {
  const q = settingsSearch.value.trim().toLowerCase()
  if (!q) return []
  return categoryEntries.value
    .map((cat) => ({
      ...cat,
      rows: cat.rows.filter(
        (s) => s.key.toLowerCase().includes(q) || (s.description || '').toLowerCase().includes(q)
      ),
    }))
    .filter((cat) => cat.rows.length > 0)
})

// What the content column renders: all matching categories while searching,
// otherwise just the category picked in the sidebar.
const displayedCategories = computed(() => {
  if (searchActive.value) return searchResults.value
  return activeCategory.value ? [activeCategory.value] : []
})

useHeaderSearch('settings', {
  query: settingsSearch,
  placeholder: 'Search all settings…',
})

const showingPatterns = computed(() => activeSection.value === 'router:patterns')
const showingRouterTest = computed(() => activeSection.value === 'router:test')

const routerMatchCount = computed(() => testResult.value?.matched_tools?.length || 0)
const patternsDirty = computed(() => patternsText.value !== patternsOriginal.value)

// Dirty rows are counted against the FULL category (not just the rows visible
// under a search filter) because Save writes the whole category.
function dirtyCount(catKey) {
  return (byCategory.value[catKey] || []).filter((s) => s.draft !== s.value).length
}

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
  patternsOriginal.value = patternsText.value

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
  settingsSearch.value = ''
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
  <div class="page-wrap settings-page">
    <div class="set-layout">
      <nav class="set-rail">
        <template
          v-for="section in sections"
          :key="section.id"
        >
          <div
            v-if="section.divider"
            class="set-rail-rule"
          />
          <button
            type="button"
            class="set-rail-btn"
            :class="{ active: section.id === activeSection }"
            @click="selectSection(section.id)"
          >
            <span class="set-rail-title">{{ section.title }}</span>
            <span class="set-rail-count">{{ section.meta }}</span>
          </button>
        </template>
      </nav>

      <div class="set-content">
        <div
          v-if="searchActive && displayedCategories.length === 0"
          class="empty-state"
        >
          <p class="empty-title">
            No matching settings
          </p>
          <p class="empty-hint">
            Nothing matches "{{ settingsSearch.trim() }}". Try a different term.
          </p>
        </div>

        <section
          v-for="cat in displayedCategories"
          :key="cat.key"
          class="panel set-panel"
        >
          <div class="panel-head">
            <h2 class="panel-title">
              {{ cat.meta.title }}
            </h2>
            <div class="set-head-actions">
              <template v-if="cat.key === 'retention'">
                <span
                  v-if="retentionResult"
                  class="set-note"
                  :title="retentionResult"
                >{{ retentionResult }}</span>
                <button
                  class="btn btn-ghost"
                  :disabled="retentionRunning"
                  @click="runRetentionNow"
                >
                  {{ retentionRunning ? 'Running…' : 'Run cleanup' }}
                </button>
              </template>
              <button
                class="btn"
                :class="dirtyCount(cat.key) ? 'btn-save' : 'btn-ghost'"
                :disabled="!dirtyCount(cat.key)"
                @click="saveCategory(cat.key)"
              >
                {{ dirtyCount(cat.key) ? `Save ${dirtyCount(cat.key)}` : 'Saved' }}
              </button>
            </div>
          </div>

          <p class="set-cat-desc">
            {{ cat.meta.description }}
          </p>

          <div class="row-list">
            <div
              v-for="s in cat.rows"
              :key="s.key"
              class="list-row set-row"
            >
              <div
                class="set-row-grid"
                :class="{ stacked: isPrompt(s.key) }"
              >
                <div class="set-copy">
                  <span class="set-key">
                    <span
                      v-if="s.draft !== s.value"
                      class="set-dirty-dot"
                    />{{ s.key }}
                  </span>
                  <p
                    v-if="s.description"
                    class="row-desc"
                  >
                    {{ s.description }}
                  </p>
                </div>

                <div class="set-control">
                  <textarea
                    v-if="isPrompt(s.key)"
                    v-model="s.draft"
                    class="set-input set-input-lg"
                    spellcheck="false"
                    placeholder="Enter prompt instructions"
                  />

                  <input
                    v-else-if="isSecret(s)"
                    v-model="s.draft"
                    type="password"
                    class="set-input"
                    placeholder="Hidden value"
                  >

                  <button
                    v-else-if="s.value_type === 'bool'"
                    class="switch"
                    type="button"
                    role="switch"
                    :aria-checked="s.draft === 'true' ? 'true' : 'false'"
                    :aria-label="`Toggle ${s.key}`"
                    @click="s.draft = s.draft === 'true' ? 'false' : 'true'"
                  />

                  <input
                    v-else-if="s.value_type === 'int'"
                    v-model="s.draft"
                    type="number"
                    step="1"
                    class="set-input"
                    placeholder="0"
                  >

                  <input
                    v-else
                    v-model="s.draft"
                    type="text"
                    class="set-input"
                    placeholder="Value"
                  >
                </div>
              </div>
            </div>
          </div>
        </section>

        <section
          v-if="!searchActive && showingPatterns"
          class="panel set-panel"
        >
          <div class="panel-head">
            <h2 class="panel-title">
              Router Patterns
            </h2>
            <button
              class="btn"
              :class="patternsDirty ? 'btn-save' : 'btn-ghost'"
              :disabled="!patternsDirty"
              @click="savePatterns"
            >
              {{ patternsDirty ? 'Save' : 'Saved' }}
            </button>
          </div>

          <p class="set-cat-desc">
            Pattern rules as JSON — each rule takes `tool_name`, `pattern`, and optional metadata.
          </p>

          <textarea
            v-model="patternsText"
            class="set-code-editor"
            spellcheck="false"
          />
        </section>

        <section
          v-if="!searchActive && showingRouterTest"
          class="panel set-panel"
        >
          <div class="panel-head">
            <h2 class="panel-title">
              Router Test
            </h2>
            <span class="panel-count">{{ routerMatchCount }} matches</span>
          </div>

          <p class="set-cat-desc">
            Check how a message is routed before it reaches the agent runtime.
          </p>

          <div class="set-test-body">
            <div class="set-test-input">
              <input
                v-model="testMsg"
                type="text"
                class="set-input"
                placeholder="Type a message to test routing"
                @keydown.enter="testRouter"
              >
              <button
                class="btn btn-primary"
                @click="testRouter"
              >
                Run
              </button>
            </div>

            <template v-if="testResult">
              <div class="set-test-row">
                <span class="set-test-label">tier</span>
                <span class="mono-chip">{{ testResult.routing_info?.tier || '?' }}</span>
              </div>
              <div class="set-test-row">
                <span class="set-test-label">tools</span>
                <div class="chip-row set-test-chips">
                  <template v-if="testResult.matched_tools?.length">
                    <span
                      v-for="t in testResult.matched_tools"
                      :key="t"
                      class="mono-chip"
                    >{{ t }}</span>
                  </template>
                  <span
                    v-else
                    class="set-test-none"
                  >none</span>
                </div>
              </div>
            </template>
            <p
              v-else
              class="set-test-hint"
            >
              Run a message through the router to inspect its tier and matching tools.
            </p>
          </div>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
.settings-page {
  padding-bottom: 60px;
}

.set-layout {
  display: grid;
  grid-template-columns: 200px minmax(0, 1fr);
  gap: 26px;
  align-items: start;
}

/* ── Rail: bare text, hairline on the right, inset accent when active ─────── */
.set-rail {
  position: sticky;
  top: 8px;
  max-height: calc(100vh - 90px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding-right: 12px;
  border-right: 1px solid var(--border);
}

.set-rail-rule {
  height: 1px;
  margin: 8px 2px;
  background: var(--border);
}

.set-rail-btn {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  padding: 6px 9px;
  border: 0;
  border-radius: var(--r-md);
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-size: 0.8rem;
  text-align: left;
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease;
}

.set-rail-btn:hover {
  background: var(--surface2);
  color: var(--text);
}

.set-rail-btn.active {
  background: color-mix(in srgb, var(--accent) 8%, transparent);
  color: var(--text);
  box-shadow: inset 2px 0 0 var(--accent);
}

.set-rail-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
}

.set-rail-btn.active .set-rail-title {
  font-weight: 600;
}

.set-rail-count {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  color: var(--muted);
  opacity: 0.7;
}

/* ── Content panels ───────────────────────────────────────────────────────── */
.set-content {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.set-head-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.set-note {
  max-width: 340px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 0.66rem;
  color: var(--muted);
}

.set-cat-desc {
  margin: 0;
  padding: 10px 16px;
  border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  font-size: 0.76rem;
  line-height: 1.55;
  color: var(--muted);
}

/* ── Setting rows ─────────────────────────────────────────────────────────── */
.set-row-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.1fr) minmax(200px, 0.9fr);
  gap: 8px 24px;
  align-items: center;
}

.set-row-grid.stacked {
  grid-template-columns: 1fr;
}

.set-copy {
  min-width: 0;
}

.set-key {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  font-family: var(--font-mono);
  font-size: 0.76rem;
  font-weight: 600;
  color: var(--text);
  overflow-wrap: anywhere;
}

.set-dirty-dot {
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: var(--accent);
  flex-shrink: 0;
}

.set-control {
  display: flex;
  justify-content: flex-end;
  min-width: 0;
}

.set-row-grid.stacked .set-control {
  justify-content: stretch;
}

.set-input {
  width: 100%;
  padding: 6px 10px;
  font-size: 0.78rem;
  font-family: var(--font-mono);
}

.set-input-lg {
  min-height: 150px;
  resize: vertical;
  line-height: 1.6;
}

/* ── Patterns editor: the panel IS the editor, no inner frame ─────────────── */
.set-code-editor {
  display: block;
  width: 100%;
  min-height: 380px;
  padding: 14px 16px;
  border: 0 !important;
  border-radius: 0 !important;
  background: transparent !important;
  font-family: var(--font-mono);
  font-size: 0.76rem;
  line-height: 1.7;
  resize: vertical;
}

.set-code-editor:focus {
  box-shadow: none !important;
}

/* ── Router test ──────────────────────────────────────────────────────────── */
.set-test-body {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 14px 16px 16px;
}

.set-test-input {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
}

.set-test-row {
  display: flex;
  align-items: baseline;
  gap: 14px;
}

.set-test-label {
  min-width: 44px;
  font-family: var(--font-mono);
  font-size: 0.66rem;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
}

.set-test-chips {
  margin-top: 0;
}

.set-test-none,
.set-test-hint {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--muted);
}

.set-test-hint {
  margin: 0;
}

@media (max-width: 960px) {
  .set-layout {
    grid-template-columns: 1fr;
    gap: 14px;
  }

  .set-rail {
    position: static;
    max-height: none;
    flex-direction: row;
    flex-wrap: wrap;
    gap: 4px;
    padding-right: 0;
    border-right: 0;
    border-bottom: 1px solid var(--border);
    padding-bottom: 10px;
  }

  .set-rail-rule {
    display: none;
  }

  .set-rail-btn {
    width: auto;
  }

  .set-rail-btn.active {
    box-shadow: inset 0 -2px 0 var(--accent);
  }

  .set-row-grid {
    grid-template-columns: 1fr;
  }

  .set-control {
    justify-content: flex-start;
  }
}
</style>
