<script setup>
import { computed, onMounted, ref } from 'vue'
import { get, post } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import Modal from './Modal.vue'

// ── Self-Improvement review panel ────────────────────────────────────────────
// Lives on the Settings page under the "Self-Improvement" rail entry. Shows the
// live state of both learnings, a one-click way to trigger them, and a review
// queue for prompt-optimizer candidates (scored revisions awaiting a human
// decision — nothing is ever auto-applied unless optimizer.auto_apply is
// explicitly enabled in the settings above).

const history = ref([])
const runs = ref([])
const status = ref(null)
const loading = ref(false)
const running = ref(false)
const sweeping = ref(false)
const preview = ref(null)
let pollTimer = null

const fewShot = computed(() => status.value?.few_shot || {})
const optimizer = computed(() => status.value?.optimizer || {})

const categories = computed(() => (fewShot.value.categories || []).map(([cat, n]) => ({ cat, n })))

// Candidates the optimizer proposed but nobody has decided on yet.
const pending = computed(() => history.value.filter((h) => h.status === 'candidate').slice(0, 20))

const applied = computed(() => history.value.find((h) => h.status === 'applied') || null)

// Historical versions worth being able to restore from.
const restorable = computed(() =>
  history.value
    .filter((h) => h.status === 'applied' || h.status === 'baseline')
    .slice(0, 10)
)

function fmt(score) {
  return score == null ? '—' : score.toFixed(1)
}

function timeAgo(iso) {
  if (!iso) return 'never'
  const then = new Date(String(iso).replace(' ', 'T') + 'Z').getTime()
  if (Number.isNaN(then)) return iso
  const mins = Math.max(0, Math.round((Date.now() - then) / 60000))
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hrs = Math.round(mins / 60)
  if (hrs < 48) return `${hrs}h ago`
  return `${Math.round(hrs / 24)}d ago`
}

function fmtPromptHead(p) {
  if (!p) return ''
  const flat = p.replace(/\s+/g, ' ').trim()
  return flat.length > 180 ? flat.slice(0, 180) + '…' : flat
}

function badgeClass(h) {
  if (h.status === 'candidate') return 'opt-badge-candidate'
  if (h.status === 'applied') return 'opt-badge-applied'
  if (h.status === 'rejected') return 'opt-badge-rejected'
  return 'opt-badge-baseline'
}

async function load() {
  loading.value = true
  try {
    const d = await get('/optimizer/history')
    status.value = d.status || null
    history.value = d.history?.history || d.history || []
    runs.value = d.runs?.optimizer_runs || d.runs || []
  } finally {
    loading.value = false
  }
}

async function runOptimizer() {
  if (running.value) return
  const before = optimizer.value.last_run?.started_at || null
  running.value = true
  const r = await post('/optimizer/run')
  if (r.error) {
    toast(r.error, false)
    running.value = false
    return
  }
  toast('Optimizer started in the background', true)
  // Poll until this run's audit row gets a finished_at (cheap status calls).
  let tries = 0
  pollTimer = setInterval(async () => {
    tries += 1
    const st = (await get('/optimizer/status')).optimizer || {}
    const last = st.last_run
    const isNew = last && last.started_at && last.started_at !== before
    if ((isNew && last.finished_at) || tries > 30) {
      clearInterval(pollTimer)
      pollTimer = null
      running.value = false
      await load()
      if (isNew) toast(`Optimizer finished: ${last.outcome}`, true)
    }
  }, 2000)
}

async function runSweep() {
  if (sweeping.value) return
  sweeping.value = true
  const r = await post('/optimizer/fewshot/sweep')
  sweeping.value = false
  if (r.error) toast(r.error, false)
  else {
    toast(r.inserted ? `Mined ${r.inserted} new example(s)` : 'No new examples to mine', true)
    await load()
  }
}

async function apply(h) {
  const ok = await confirmDialog(
    `Load prompt #${h.id} as the live system prompt? The current prompt is snapshotted first, so this is reversible from the list below.`,
    { title: 'Apply prompt revision', confirmText: 'Apply' }
  )
  if (!ok) return
  const r = await post(`/optimizer/apply/${h.id}`)
  if (r.error) toast(r.error, false)
  else {
    toast(`Prompt #${h.id} is now live`, true)
    await load()
  }
}

async function reject(h) {
  if (h.status !== 'candidate') return
  const r = await post(`/optimizer/reject/${h.id}`)
  if (r.error) toast(r.error, false)
  else {
    toast(`Candidate #${h.id} dismissed`, true)
    await load()
  }
}

onMounted(load)
</script>

<template>
  <div class="opt-root">
    <div class="panel-head">
      <h2 class="panel-title">
        Self-Improvement
      </h2>
      <div class="set-head-actions">
        <span
          v-if="loading"
          class="set-note"
        >loading…</span>
        <button
          class="btn btn-ghost"
          :disabled="loading"
          @click="load"
        >
          Refresh
        </button>
      </div>
    </div>

    <p class="set-cat-desc">
      Two ways Axon learns from its own history, both on by default and gated by
      the <code>agent.few_shot.*</code> and <code>optimizer.*</code> settings
      above. <strong>Few-shot</strong> replays your best completed runs as
      reference examples on similar future tasks. <strong>Optimizer</strong>
      scans recent runs for failures and drafts rewritten system prompts that
      would fix them, scored 0–10. Nothing is applied automatically — candidates
      wait here for you — unless <code>auto_apply</code> is enabled.
    </p>

    <!-- Overview strip -->
    <div class="opt-overview">
      <div class="opt-cell">
        <span class="opt-cell-label">few-shot</span>
        <span class="opt-cell-value">
          {{ fewShot.example_count ?? 0 }}
          <span class="opt-cell-sub">examples</span>
        </span>
        <div
          v-if="categories.length"
          class="chip-row opt-chips"
        >
          <span
            v-for="c in categories.slice(0, 8)"
            :key="c.cat"
            class="mono-chip"
          >{{ c.cat }} · {{ c.n }}</span>
        </div>
        <span
          v-else
          class="opt-cell-sub"
        >mining starts with the first clean, tool-backed completed run</span>
      </div>

      <div class="opt-cell">
        <span class="opt-cell-label">optimizer</span>
        <span class="opt-cell-value">
          {{ optimizer.enabled ? (optimizer.auto_apply ? 'auto-apply' : 'proposals') : 'disabled' }}
          <span class="opt-cell-sub">
            · every {{ optimizer.interval_hours }}h · threshold {{ optimizer.min_score }}/10
          </span>
        </span>
        <span class="opt-cell-sub">
          last run
          {{ optimizer.last_run ? `${timeAgo(optimizer.last_run.started_at)} — ${optimizer.last_run.outcome || 'queued'}` : 'never' }}
        </span>
      </div>

      <div class="opt-cell">
        <span class="opt-cell-label">current prompt</span>
        <span
          v-if="optimizer.applied_prompt_id != null"
          class="opt-cell-value opt-prompt-live"
        >
          #{{ optimizer.applied_prompt_id }}
        </span>
        <span
          v-else
          class="opt-cell-sub"
        >no promoted revision yet — the seed system prompt is live</span>
        <span
          v-if="optimizer.applied_at"
          class="opt-cell-sub"
        >promoted {{ timeAgo(optimizer.applied_at) }} · head: {{ fmtPromptHead(optimizer.applied_prompt_head) }}</span>
      </div>

      <div class="opt-actions">
        <button
          class="btn btn-primary"
          :disabled="running || !optimizer.enabled"
          @click="runOptimizer"
        >
          {{ running ? 'Optimizing…' : 'Run optimizer' }}
        </button>
        <button
          class="btn btn-ghost"
          :disabled="sweeping || !fewShot.enabled"
          @click="runSweep"
        >
          {{ sweeping ? 'Mining…' : 'Mine examples' }}
        </button>
      </div>
    </div>

    <!-- Review queue -->
    <div class="opt-section-label">
      Awaiting your decision
      <span class="opt-section-count">{{ pending.length }}</span>
    </div>

    <div class="row-list">
      <div
        v-if="!pending.length"
        class="opt-empty"
      >
        <p class="empty-title">
          No proposals yet
        </p>
        <p class="empty-hint">
          Candidates appear here after an optimizer run finds a failure worth
          fixing. Apply the ones you like, reject the rest.
        </p>
      </div>

      <div
        v-for="h in pending"
        :key="h.id"
        class="list-row opt-row"
      >
        <div class="opt-row-main">
          <div class="opt-row-top">
            <span class="mono-chip">#{{ h.id }}</span>
            <span class="set-key">
              score
              <span class="opt-score">{{ fmt(h.score) }}</span>/10
            </span>
            <span class="row-desc">
              {{ timeAgo(h.created_at) }}
            </span>
          </div>
          <p class="row-desc opt-rationale">
            <strong>why:</strong>
            {{ h.rationale || 'no rationale given' }}
          </p>
          <ul
            v-if="h.failures?.length"
            class="opt-failures"
          >
            <li
              v-for="(f, i) in h.failures"
              :key="i"
            >
              <span class="opt-fail-task">{{ f.task }}</span>
              <span class="opt-fail-issue">{{ f.issue }}</span>
            </li>
          </ul>
          <button
            class="btn btn-ghost btn-sm opt-diff-btn"
            @click="preview = h"
          >
            Preview prompt
          </button>
        </div>
        <div class="opt-row-actions">
          <button
            class="btn btn-save btn-sm"
            @click="apply(h)"
          >
            Apply
          </button>
          <button
            class="btn btn-ghost btn-sm"
            @click="reject(h)"
          >
            Reject
          </button>
        </div>
      </div>
    </div>

    <!-- Restorable history -->
    <template v-if="restorable.length">
      <div class="opt-section-label">
        Applied / restorable versions
      </div>
      <div class="row-list">
        <div
          v-for="h in restorable"
          :key="h.id"
          class="list-row opt-row opt-row-hist"
        >
          <div class="opt-row-main">
            <div class="opt-row-top">
              <span
                class="mono-chip"
                :class="badgeClass(h)"
              >{{ h.status }}</span>
              <span class="mono-chip">#{{ h.id }}</span>
              <span
                v-if="h.score != null"
                class="mono-chip"
              >score {{ fmt(h.score) }}</span>
              <span class="row-desc">{{ timeAgo(h.created_at) }}</span>
            </div>
            <span class="opt-history-head">{{ fmtPromptHead(h.prompt) }}</span>
            <button
              class="btn btn-ghost btn-sm opt-diff-btn"
              @click="preview = h"
            >
              Preview
            </button>
          </div>
          <div
            v-if="h.status !== 'applied'"
            class="opt-row-actions"
          >
            <button
              class="btn btn-save btn-sm"
              @click="apply(h)"
            >
              Restore
            </button>
          </div>
        </div>
      </div>
    </template>

    <!-- Run history -->
    <template v-if="runs.length">
      <div class="opt-section-label">
        Optimizer runs
      </div>
      <div class="row-list">
        <div
          v-for="r in runs.slice(0, 8)"
          :key="r.id"
          class="list-row opt-row opt-row-hist"
        >
          <div class="opt-row-main">
            <div class="opt-row-top">
              <span class="mono-chip">#{{ r.id }}</span>
              <span class="row-desc">{{ timeAgo(r.started_at) }}</span>
            </div>
            <span class="opt-history-head">
              {{ r.outcome || r.error || 'in progress' }}
              <template v-if="r.failures_count != null">
                ({{ r.failures_count }} failures, {{ r.candidates_count }} candidates)
              </template>
            </span>
          </div>
        </div>
      </div>
    </template>

    <!-- Full-prompt preview -->
    <Modal
      v-model="preview"
      :title="`Prompt #${preview?.id} (${preview?.status})`"
      max-width="860px"
    >
      <template v-if="preview">
        <p class="opt-modal-meta">
          score {{ fmt(preview.score) }}/10 · {{ preview.rationale || 'no rationale' }}
        </p>
        <pre
          class="opt-prompt-pre"
          spellcheck="false"
        >{{ preview.prompt }}</pre>
        <div class="opt-modal-actions">
          <template v-if="preview.status === 'candidate'">
            <button
              class="btn btn-ghost"
              @click="preview = null"
            >
              Close
            </button>
            <button
              class="btn btn-save"
              @click="apply(preview); preview = null"
            >
              Apply revision
            </button>
          </template>
          <button
            v-else
            class="btn btn-primary"
            @click="preview = null"
          >
            Close
          </button>
        </div>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
/* Shared settings-panel bits that live in SettingsPage's scoped styles — this
   component is rendered as a child, so it must define them itself. */
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

.set-cat-desc strong {
  color: var(--text);
  font-weight: 600;
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

.opt-root {
  display: flex;
  flex-direction: column;
}

.opt-overview {
  display: grid;
  grid-template-columns: 1.2fr 1fr 1fr auto;
  gap: 18px;
  padding: 14px 16px;
  border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
}

.opt-cell {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.opt-cell-label {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}

.opt-cell-value {
  font-size: 0.95rem;
  font-weight: 650;
  color: var(--text);
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.opt-cell-sub {
  font-family: var(--font-mono);
  font-size: 0.64rem;
  line-height: 1.5;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.opt-prompt-live {
  color: var(--accent);
}

.opt-chips {
  margin-top: 2px;
  flex-wrap: wrap;
}

.opt-actions {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  justify-content: center;
  gap: 8px;
}

.opt-section-label {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 16px 4px;
  font-family: var(--font-mono);
  font-size: 0.66rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}

.opt-section-count {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
  border-radius: 999px;
  padding: 0 8px;
  font-size: 0.62rem;
}

.opt-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.opt-row-hist {
  align-items: center;
}

.opt-row-main {
  min-width: 0;
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 5px;
}

.opt-row-top {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.opt-score {
  font-family: var(--font-mono);
  font-weight: 700;
  color: var(--accent);
}

.opt-rationale {
  margin: 0;
}

.opt-failures {
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.opt-failures li {
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 1px;
  font-size: 0.72rem;
}

.opt-fail-task {
  font-family: var(--font-mono);
  font-weight: 600;
  color: var(--text);
  overflow-wrap: anywhere;
}

.opt-fail-issue {
  color: var(--muted);
  overflow-wrap: anywhere;
}

.opt-diff-btn {
  align-self: flex-start;
  opacity: 0;
  transition: opacity 0.15s var(--ease-out);
}

.opt-row:hover .opt-diff-btn {
  opacity: 1;
}

.opt-row-actions {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-shrink: 0;
}

.opt-history-head {
  font-family: var(--font-mono);
  font-size: 0.72rem;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.opt-empty {
  flex: 1;
  padding: 16px;
}

.opt-badge-applied {
  color: var(--success, #4ade80);
}

.opt-badge-candidate {
  color: var(--accent);
}

.opt-badge-rejected {
  opacity: 0.55;
}

.opt-badge-baseline {
  opacity: 0.75;
}

.opt-modal-meta {
  margin: 0 0 10px;
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--muted);
}

.opt-prompt-pre {
  max-height: 55vh;
  overflow: auto;
  margin: 0;
  padding: 12px 14px;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  background: var(--surface2);
  font-family: var(--font-mono);
  font-size: 0.74rem;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--text);
}

.opt-modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 14px;
}

@media (max-width: 1100px) {
  .opt-overview {
    grid-template-columns: 1fr 1fr;
  }
}

@media (max-width: 720px) {
  .opt-overview {
    grid-template-columns: 1fr;
  }
}
</style>