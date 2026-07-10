<script setup>
import { computed, onMounted, ref } from 'vue'
import { del, get, post } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { fmtTokens, safeJsonParse, timeAgo } from '../lib/utils.js'
import Pill from '../components/Pill.vue'
import SearchInput from '../components/SearchInput.vue'
import { useHeaderSearch } from '../lib/headerSearch.js'

const stmRuns = ref([])
const ltmEntries = ref([])
const ltmSearch = ref('')
const stmSearch = ref('')

// The topbar field filters the run list live; long-term memory keeps its own
// bar below because it's a server-side semantic search with explicit buttons.
useHeaderSearch('memories', {
  query: stmSearch,
  placeholder: 'Search runs by task…',
})
const expandedRuns = ref(new Set())
const loadedTraces = ref({})

const stmBadge = computed(() => stmRuns.value.length)
const ltmBadge = computed(() => ltmEntries.value.length)
const filteredStmRuns = computed(() => {
  const q = stmSearch.value.trim().toLowerCase()
  if (!q) return stmRuns.value
  return stmRuns.value.filter((r) => String(r.task || '').toLowerCase().includes(q))
})

async function loadSTM() {
  const d = await get('/runs')
  stmRuns.value = Array.isArray(d) ? d : (d.runs ?? [])
}

async function loadLTM() {
  const d = await get('/memory/recent?n=30')
  ltmEntries.value = Array.isArray(d) ? d : (d.memories ?? d.entries ?? [])
}

async function searchLTM() {
  if (!ltmSearch.value.trim()) return loadLTM()
  const d = await post('/memory/search', { query: ltmSearch.value, top_k: 20 })
  ltmEntries.value = Array.isArray(d) ? d : (d.results ?? [])
}

async function deleteMemory(id) {
  const ok = await confirmDialog('This memory will be permanently deleted. This action cannot be undone.', {
    title: 'Delete Memory',
    confirmText: 'Delete',
  })
  if (!ok) return
  const r = await del(`/memory/${id}`)
  toast(r.ok ? 'Deleted' : r.error, r.ok)
  if (ltmSearch.value.trim()) searchLTM()
  else loadLTM()
}

function toggleRun(id) {
  if (expandedRuns.value.has(id)) {
    expandedRuns.value.delete(id)
  } else {
    expandedRuns.value.add(id)
  }
}

async function loadTrace(id) {
  const d = await get(`/runs/${id}`)
  loadedTraces.value = { ...loadedTraces.value, [id]: d }
}

function getModels(r) {
  return safeJsonParse(r.models_used, []).join(', ')
}

function getTools(r) {
  return safeJsonParse(r.tools_used, []).join(', ')
}

function statusType(s) {
  if (s === 'completed') return 'ok'
  if (s === 'running') return 'info'
  return 'err'
}

onMounted(() => {
  loadSTM()
  loadLTM()
})
</script>

<template>
  <div class="services-page memories-page">
    <div class="memory-grid">
      <section class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <h2>Short-Term Memory</h2>
          </div>
          <span class="card-summary">{{ stmBadge }} runs</span>
        </div>

        <div class="action-bar gap-12">
          <button
            class="btn btn-ghost"
            @click="loadSTM"
          >
            Refresh
          </button>
        </div>

        <div class="service-list">
          <div
            v-for="r in filteredStmRuns"
            :key="r.id"
            class="service-item run-item-row"
            :class="{ expanded: expandedRuns.has(r.id) }"
          >
            <div class="service-info">
              <div
                class="service-name-row clickable"
                @click="toggleRun(r.id)"
              >
                <div class="service-name-group">
                  <span class="run-task-text">{{ String(r.task || 'No task description').slice(0, 100) }}</span>
                </div>
                <div class="service-actions run-actions-right">
                  <Pill
                    :type="statusType(r.status)"
                    :text="r.status"
                  />
                  <Pill
                    type="muted"
                    :text="r.platform"
                  />
                  <span class="time-text">{{ timeAgo(r.created_at) }}</span>
                  <svg
                    class="collapse-icon"
                    :class="{ rotated: expandedRuns.has(r.id) }"
                    viewBox="0 0 24 24"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
                    <path
                      d="m9 6 6 6-6 6"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                </div>
              </div>

              <div
                v-if="expandedRuns.has(r.id)"
                class="run-expanded-content"
              >
                <div class="run-meta-grid">
                  <div class="meta-item">
                    <span class="meta-label">Iterations</span>
                    <span class="meta-value">{{ r.iterations }}</span>
                  </div>
                  <div class="meta-item">
                    <span class="meta-label">Tokens</span>
                    <span class="meta-value">{{ fmtTokens(r.total_tokens) }}</span>
                  </div>
                  <div class="meta-item">
                    <span class="meta-label">Models</span>
                    <span class="meta-value">{{ getModels(r) || '-' }}</span>
                  </div>
                  <div class="meta-item">
                    <span class="meta-label">Tools</span>
                    <span class="meta-value">{{ getTools(r) || '-' }}</span>
                  </div>
                </div>

                <div
                  v-if="r.result"
                  class="run-result-box"
                >
                  <div class="box-label">
                    Output Result
                  </div>
                  <pre class="result-text">{{ String(r.result) }}</pre>
                </div>

                <button
                  class="btn btn-sm btn-ghost trace-btn"
                  @click="loadTrace(r.id)"
                >
                  {{ loadedTraces[r.id] ? 'Refresh Trace' : 'Load Tool Trace' }}
                </button>

                <div
                  v-if="loadedTraces[r.id]"
                  class="trace-explorer"
                >
                  <div
                    v-if="loadedTraces[r.id].iterations?.length"
                    class="trace-section"
                  >
                    <div class="section-header">
                      ITERATION HISTORY
                    </div>
                    <div class="trace-timeline">
                      <div
                        v-for="it in loadedTraces[r.id].iterations"
                        :key="it.iteration"
                        class="trace-log-entry"
                      >
                        <div class="entry-header">
                          <span class="entry-title">Iteration {{ it.iteration }}</span>
                          <span class="entry-meta">{{ it.duration_ms != null ? (it.duration_ms / 1000).toFixed(1) + 's' : '?s' }}</span>
                        </div>
                        <div class="entry-body">
                          Model: <code>{{ it.model_name }}</code> | Tokens: {{ it.tokens }}
                        </div>
                      </div>
                    </div>
                  </div>

                  <div
                    v-if="loadedTraces[r.id].tool_calls?.length"
                    class="trace-section"
                  >
                    <div class="section-header">
                      TOOL CALLS
                    </div>
                    <div class="tool-calls-list">
                      <div
                        v-for="(tc, i) in loadedTraces[r.id].tool_calls"
                        :key="i"
                        class="tool-call-entry"
                      >
                        <div class="entry-header">
                          <span class="tool-name">{{ tc.tool_name }}</span>
                          <div class="entry-actions">
                            <span class="entry-meta">{{ tc.duration_ms || 0 }}ms {{ tc.parallel ? 'parallel' : '' }}</span>
                            <Pill
                              :type="tc.error ? 'err' : 'ok'"
                              :text="tc.error ? 'error' : 'ok'"
                            />
                          </div>
                        </div>
                        <div class="entry-body">
                          <div
                            v-if="tc.args"
                            class="code-snippet"
                          >
                            <div class="snippet-label">
                              Arguments
                            </div>
                            <pre><code>{{ String(tc.args) }}</code></pre>
                          </div>
                          <div
                            v-if="tc.result"
                            class="code-snippet result"
                          >
                            <div class="snippet-label">
                              Result
                            </div>
                            <pre><code>{{ String(tc.result) }}</code></pre>
                          </div>
                          <div
                            v-if="tc.error"
                            class="error-text"
                          >
                            Error: {{ tc.error }}
                          </div>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>
          <div
            v-if="filteredStmRuns.length === 0"
            class="empty-state"
          >
            {{ stmSearch.trim() ? 'No runs match your search.' : 'No short-term memories found.' }}
          </div>
        </div>
      </section>

      <section class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <h2>Long-Term Memory</h2>
          </div>
          <span class="card-summary">{{ ltmBadge }} entries</span>
        </div>

        <div class="action-bar search-bar-row">
          <SearchInput
            v-model="ltmSearch"
            :autofocus="false"
            placeholder="Search persistent knowledge…"
            @keydown.enter="searchLTM"
          />
          <button
            class="btn btn-save"
            @click="searchLTM"
          >
            Search
          </button>
          <button
            class="btn btn-ghost"
            @click="loadLTM"
          >
            Load Recent
          </button>
        </div>

        <div class="service-list">
          <div
            v-for="e in ltmEntries"
            :key="e.id"
            class="service-item memory-entry"
          >
            <div class="memory-top-row">
              <div class="memory-content-text">
                {{ e.content }}
              </div>
              <button
                class="btn btn-sm btn-ghost btn-icon delete-btn-top"
                @click="deleteMemory(e.id)"
              >
                x
              </button>
            </div>
            <div class="memory-meta-row">
              <span class="source-pill">{{ e.source || 'Unknown Source' }}</span>
              <span class="divider">|</span>
              <span class="time-text">{{ timeAgo(e.created_at) }}</span>
              <template v-if="e.score != null">
                <span class="divider">|</span>
                <span class="match-score">{{ (e.score * 100).toFixed(0) }}% match</span>
              </template>
            </div>
          </div>
          <div
            v-if="ltmEntries.length === 0"
            class="empty-state"
          >
            No knowledge entries found. Try searching or load recent.
          </div>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.memories-page {
  padding-bottom: 60px;
}

.memory-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
  align-items: start;
}

.action-bar {
  padding: 16px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.05);
  display: flex;
  justify-content: space-between;
}

.gap-12 {
  gap: 12px;
}

.run-item-row .clickable {
  cursor: pointer;
}

.run-task-text {
  font-size: 15px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
}

.run-item-row .service-name-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 20px;
}

.service-name-group {
  display: flex;
  align-items: center;
  flex: 1;
  min-width: 0;
}

.run-actions-right {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-shrink: 0;
}

.time-text {
  font-size: 12px;
  color: var(--muted);
}

.collapse-icon {
  flex-shrink: 0;
  opacity: 0.6;
  transition: transform 0.15s ease;
}

.collapse-icon.rotated {
  transform: rotate(90deg);
}

.run-expanded-content {
  margin-top: 20px;
  padding-top: 20px;
  border-top: 1px solid rgba(0, 0, 0, 0.05);
}

.run-meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 14px;
  margin-bottom: 20px;
}

.run-result-box {
  margin-bottom: 16px;
}

.result-text {
  font-size: 13px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 280px;
  overflow-y: auto;
}

.search-bar-row {
  padding: 16px;
}

.memory-entry {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.memory-top-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.delete-btn-top {
  opacity: 0.5;
  flex-shrink: 0;
}

.memory-meta-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.match-score {
  font-size: 11px;
  font-weight: 700;
  color: var(--teal);
}

.empty-state {
  padding: 40px;
  text-align: center;
  color: var(--muted);
  font-size: 14px;
}

@media (max-width: 960px) {
  .memory-grid {
    grid-template-columns: 1fr;
  }

  .action-bar,
  .search-bar-row {
    flex-direction: column;
    gap: 12px;
  }

  .run-item-row .service-name-row {
    flex-direction: column;
    align-items: flex-start;
  }

  .run-actions-right {
    flex-wrap: wrap;
  }
}
</style>
