<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { del, get, post } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { fmtTokens, safeJsonParse, timeAgo } from '../lib/utils.js'
import { useHeaderSearch } from '../lib/headerSearch.js'

const stmRuns = ref([])
const ltmEntries = ref([])
const memSearch = ref('')

// One topbar field searches both columns: it filters the run list live, and
// Enter runs the server-side semantic search over long-term memory.
useHeaderSearch('memories', {
  query: memSearch,
  placeholder: 'Search memories… (Enter searches long-term)',
  onSubmit: searchLTM,
})
const expandedRuns = ref(new Set())
const loadedTraces = ref({})

const filteredStmRuns = computed(() => {
  const q = memSearch.value.trim().toLowerCase()
  if (!q) return stmRuns.value
  return stmRuns.value.filter((r) => String(r.task || '').toLowerCase().includes(q))
})

// Clearing the field resets long-term memory back to the recent feed.
watch(memSearch, (q) => {
  if (!q.trim()) loadLTM()
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
  if (!memSearch.value.trim()) return loadLTM()
  const d = await post('/memory/search', { query: memSearch.value, top_k: 20 })
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
  if (memSearch.value.trim()) searchLTM()
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

function stateKey(s) {
  if (s === 'completed') return 'ok'
  if (s === 'running') return 'run'
  return 'err'
}

onMounted(() => {
  loadSTM()
  loadLTM()
})
</script>

<template>
  <div class="page-wrap memories-page">
    <div class="memory-grid">
      <section class="panel">
        <div class="panel-head">
          <h2 class="panel-title">
            Short-term memory
          </h2>
          <div class="head-actions">
            <span class="panel-count">{{ filteredStmRuns.length }} runs</span>
            <button
              class="btn btn-ghost"
              @click="loadSTM"
            >
              Refresh
            </button>
          </div>
        </div>

        <div class="row-list">
          <div
            v-for="r in filteredStmRuns"
            :key="r.id"
            class="list-row run-row"
          >
            <div
              class="row-line run-line"
              role="button"
              tabindex="0"
              @click="toggleRun(r.id)"
              @keydown.enter="toggleRun(r.id)"
            >
              <div class="run-ident">
                <span
                  class="state-dot"
                  :class="stateKey(r.status)"
                  :title="r.status"
                />
                <span class="run-task-text">{{ String(r.task || 'No task description').slice(0, 100) }}</span>
              </div>
              <div class="run-meta">
                <span
                  class="run-state-label"
                  :class="stateKey(r.status)"
                >{{ r.status }}</span>
                <span class="run-time">{{ r.platform }} · {{ timeAgo(r.created_at) }}</span>
                <svg
                  class="collapse-icon"
                  :class="{ rotated: expandedRuns.has(r.id) }"
                  viewBox="0 0 24 24"
                  width="13"
                  height="13"
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
              class="run-expanded"
            >
              <p class="run-facts">
                {{ r.iterations }} iterations · {{ fmtTokens(r.total_tokens) }} tokens
                <template v-if="getModels(r)">
                  · {{ getModels(r) }}
                </template>
              </p>
              <p
                v-if="getTools(r)"
                class="run-facts"
              >
                tools: {{ getTools(r) }}
              </p>

              <div
                v-if="r.result"
                class="run-result"
              >
                <span class="block-label">result</span>
                <pre class="result-text">{{ String(r.result) }}</pre>
              </div>

              <button
                class="btn btn-xs btn-ghost"
                @click="loadTrace(r.id)"
              >
                {{ loadedTraces[r.id] ? 'Refresh trace' : 'Load tool trace' }}
              </button>

              <div
                v-if="loadedTraces[r.id]"
                class="trace-explorer"
              >
                <div
                  v-if="loadedTraces[r.id].iterations?.length"
                  class="trace-section"
                >
                  <span class="block-label">iteration history</span>
                  <div
                    v-for="it in loadedTraces[r.id].iterations"
                    :key="it.iteration"
                    class="trace-entry"
                  >
                    <div class="trace-entry-head">
                      <span class="trace-entry-title">iteration {{ it.iteration }}</span>
                      <span class="trace-entry-meta">{{ it.duration_ms != null ? (it.duration_ms / 1000).toFixed(1) + 's' : '?s' }}</span>
                    </div>
                    <p class="trace-entry-body">
                      {{ it.model_name }} · {{ it.tokens }} tokens
                    </p>
                  </div>
                </div>

                <div
                  v-if="loadedTraces[r.id].tool_calls?.length"
                  class="trace-section"
                >
                  <span class="block-label">tool calls</span>
                  <div
                    v-for="(tc, i) in loadedTraces[r.id].tool_calls"
                    :key="i"
                    class="trace-entry"
                  >
                    <div class="trace-entry-head">
                      <span
                        class="trace-entry-title"
                        :class="{ err: tc.error }"
                      >{{ tc.tool_name }}</span>
                      <span class="trace-entry-meta">
                        {{ tc.duration_ms || 0 }}ms{{ tc.parallel ? ' · parallel' : '' }} · {{ tc.error ? 'error' : 'ok' }}
                      </span>
                    </div>
                    <pre
                      v-if="tc.args"
                      class="trace-snippet"
                    >{{ String(tc.args) }}</pre>
                    <pre
                      v-if="tc.result"
                      class="trace-snippet"
                    >{{ String(tc.result) }}</pre>
                    <p
                      v-if="tc.error"
                      class="trace-error"
                    >
                      {{ tc.error }}
                    </p>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <div
            v-if="filteredStmRuns.length === 0"
            class="panel-empty"
          >
            {{ memSearch.trim() ? 'No runs match your search.' : 'No short-term memories yet.' }}
          </div>
        </div>
      </section>

      <section class="panel">
        <div class="panel-head">
          <h2 class="panel-title">
            Long-term memory
          </h2>
          <div class="head-actions">
            <span class="panel-count">{{ ltmEntries.length }} entries</span>
            <button
              class="btn btn-ghost"
              @click="loadLTM"
            >
              Load recent
            </button>
          </div>
        </div>

        <div class="row-list">
          <div
            v-for="e in ltmEntries"
            :key="e.id"
            class="list-row memory-row"
          >
            <div class="row-line memory-line">
              <p class="memory-content">
                {{ e.content }}
              </p>
              <button
                class="btn btn-xs btn-danger row-action"
                title="Delete memory"
                @click="deleteMemory(e.id)"
              >
                Delete
              </button>
            </div>
            <p class="memory-readout">
              <span class="mono-chip">{{ e.source || 'unknown' }}</span>
              {{ timeAgo(e.created_at) }}
              <template v-if="e.score != null">
                · <span class="memory-score">{{ (e.score * 100).toFixed(0) }}% match</span>
              </template>
            </p>
          </div>

          <div
            v-if="ltmEntries.length === 0"
            class="panel-empty"
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
  gap: 14px;
  align-items: start;
}

.head-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.panel-empty {
  padding: 32px 16px;
  text-align: center;
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--muted);
}

/* ── Run rows ─────────────────────────────────────────────────────────────── */
.run-line {
  cursor: pointer;
}

.run-ident {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  flex: 1;
}

.state-dot {
  width: 7px;
  height: 7px;
  border-radius: 999px;
  flex-shrink: 0;
}

.state-dot.ok { background: var(--green); }
.state-dot.run { background: var(--blue); }
.state-dot.err { background: var(--red); }

.run-task-text {
  font-size: 0.8rem;
  font-weight: 600;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.run-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}

.run-state-label {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}

.run-state-label.err { color: var(--red); }
.run-state-label.run { color: var(--blue); }

.run-time {
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
  white-space: nowrap;
}

.collapse-icon {
  flex-shrink: 0;
  color: var(--muted);
  opacity: 0.6;
  transition: transform 0.15s ease;
}

.collapse-icon.rotated {
  transform: rotate(90deg);
}

/* ── Expanded run detail ──────────────────────────────────────────────────── */
.run-expanded {
  margin-top: 10px;
  padding-top: 10px;
  border-top: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
}

.run-facts {
  margin: 0 0 8px;
  font-family: var(--font-mono);
  font-size: 0.68rem;
  line-height: 1.6;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.block-label {
  display: block;
  margin-bottom: 6px;
  font-family: var(--font-mono);
  font-size: 0.6rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}

.run-result {
  margin-bottom: 10px;
}

.result-text {
  margin: 0;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  background: color-mix(in srgb, var(--text) 2.5%, transparent);
  font-family: var(--font-mono);
  font-size: 0.7rem;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 280px;
  overflow-y: auto;
}

.trace-explorer {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.trace-entry {
  padding: 8px 10px;
  border: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
  border-radius: var(--r-md);
  margin-bottom: 6px;
}

.trace-entry-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
}

.trace-entry-title {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  font-weight: 600;
  color: var(--text);
}

.trace-entry-title.err {
  color: var(--red);
}

.trace-entry-meta {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  color: var(--muted);
  white-space: nowrap;
}

.trace-entry-body {
  margin: 4px 0 0;
  font-family: var(--font-mono);
  font-size: 0.66rem;
  color: var(--muted);
}

.trace-snippet {
  margin: 6px 0 0;
  padding: 8px 10px;
  border-radius: var(--r-sm);
  background: color-mix(in srgb, var(--text) 3%, transparent);
  font-family: var(--font-mono);
  font-size: 0.64rem;
  line-height: 1.55;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 160px;
  overflow-y: auto;
}

.trace-error {
  margin: 6px 0 0;
  font-family: var(--font-mono);
  font-size: 0.66rem;
  color: var(--red);
}

/* ── Long-term rows ───────────────────────────────────────────────────────── */
.memory-line {
  align-items: flex-start;
}

.memory-content {
  margin: 0;
  flex: 1;
  min-width: 0;
  font-size: 0.8rem;
  line-height: 1.55;
  color: var(--text);
  overflow-wrap: anywhere;
}

.row-action {
  opacity: 0.25;
  transition: opacity 0.15s ease;
}

.memory-row:hover .row-action,
.row-action:focus-visible {
  opacity: 1;
}

@media (hover: none) {
  .row-action {
    opacity: 1;
  }
}

.memory-readout {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin: 8px 0 0;
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
}

.memory-score {
  color: var(--accent);
  font-weight: 600;
}

@media (max-width: 960px) {
  .memory-grid {
    grid-template-columns: 1fr;
  }
}
</style>
