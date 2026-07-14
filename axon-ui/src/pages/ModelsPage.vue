<script setup>
import { ref, computed, onMounted, watch } from 'vue'
import { get, post, put, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { fmtTokens, timeAgo } from '../lib/utils.js'
import Modal from '../components/Modal.vue'
import SearchableSelect from '../components/SearchableSelect.vue'
import { useHeaderSearch } from '../lib/headerSearch.js'

const models = ref([])
const modelSearch = ref('')

useHeaderSearch('models', {
  query: modelSearch,
  placeholder: 'Search models by name or role…',
  visible: computed(() => models.value.length > 0),
})
const filteredModels = computed(() => {
  const q = modelSearch.value.trim().toLowerCase()
  if (!q) return models.value
  return models.value.filter(
    (m) => m.name.toLowerCase().includes(q) || (m.role || '').toLowerCase().includes(q)
  )
})
const modalOpen = ref(false)
const editing = ref(false)
const form = ref({
  name: '',
  provider: 'openai',
  model_id: '',
  api_key: '',
  base_url: '',
  priority: 1,
  role: '',
  max_tokens: 4096,
})

const PROVIDERS = [
  { value: 'openai', label: 'OpenAI' },
  { value: 'google', label: 'Google Gemini' },
  { value: 'anthropic', label: 'Anthropic' },
  { value: 'groq', label: 'Groq' },
  { value: 'cerebras', label: 'Cerebras' },
  { value: 'nvidia', label: 'NVIDIA' },
  { value: 'openrouter', label: 'OpenRouter' },
  { value: 'ollama', label: 'Ollama' },
]

// Live Model-ID dropdown: options are the provider's models, prefetched daily
// server-side into a cache and read (never a live provider call) via this
// endpoint. Free text always overrides — this only offers suggestions.
const availableModels = ref([])
const modelsLoading = ref(false)

// Shape the prefetched models for SearchableSelect: a compact, searchable,
// in-place dropdown that closes on pick and lets any ID be typed as an override.
const modelIdOptions = computed(() =>
  availableModels.value.map((o) => ({
    value: o.id,
    name: o.id,
    description: o.label && o.label !== o.id ? o.label : '',
  }))
)
let modelsFetchSeq = 0
let modelsFetchTimer = null

async function fetchAvailableModels() {
  const provider = form.value.provider
  if (!provider) {
    availableModels.value = []
    return
  }
  const seq = ++modelsFetchSeq
  modelsLoading.value = true
  try {
    const r = await post('/models/available', {
      provider,
      base_url: form.value.base_url || '',
      // On edit, let the backend reuse the model's own base_url grouping.
      name: editing.value ? form.value.name : undefined,
    })
    if (seq !== modelsFetchSeq) return // superseded by a newer request
    availableModels.value = r && r.ok && Array.isArray(r.models) ? r.models : []
  } catch {
    if (seq === modelsFetchSeq) availableModels.value = []
  } finally {
    if (seq === modelsFetchSeq) modelsLoading.value = false
  }
}

function scheduleFetchModels() {
  clearTimeout(modelsFetchTimer)
  modelsFetchTimer = setTimeout(fetchAvailableModels, 400)
}

// Refetch when the provider or base URL changes while the modal is open.
watch(
  () => [form.value.provider, form.value.base_url],
  () => {
    if (modalOpen.value) scheduleFetchModels()
  }
)

async function load() {
  const d = await get('/models')
  models.value = d.models || []
}

function showAdd() {
  editing.value = false
  form.value = {
    name: '',
    provider: 'openai',
    model_id: '',
    api_key: '',
    base_url: '',
    priority: 1,
    role: '',
    max_tokens: 4096,
  }
  availableModels.value = []
  modalOpen.value = true
  fetchAvailableModels()
}

function showEdit(m) {
  editing.value = true
  form.value = {
    name: m.name,
    provider: m.provider,
    model_id: m.model_id,
    api_key: '',
    base_url: m.base_url || '',
    priority: m.priority,
    role: m.role || '',
    max_tokens: m.max_tokens || 4096,
  }
  availableModels.value = []
  modalOpen.value = true
  fetchAvailableModels()
}

async function save() {
  if (!form.value.name || (!editing.value && !form.value.api_key)) {
    return toast('Name and API key are required for new models', false)
  }
  const r = editing.value
    ? await put(`/models/${encodeURIComponent(form.value.name)}`, form.value)
    : await post('/models', form.value)
  toast(r.ok ? 'Model saved' : r.error, r.ok)
  if (r.ok) {
    modalOpen.value = false
    load()
  }
}

async function toggle(m) {
  const r = await put(`/models/${encodeURIComponent(m.name)}`, {
    enabled: m.enabled === false,
  })
  toast(r.ok ? 'Updated' : r.error, r.ok)
  load()
}

async function enableAll() {
  const names = models.value.filter(m => m.enabled === false).map(m => m.name)
  if (names.length === 0) return
  const r = await put('/models/bulk', { names, enabled: true })
  toast(r.ok ? 'Enabled all models' : r.error, r.ok)
  load()
}

async function disableAll() {
  const names = models.value.filter(m => m.enabled !== false).map(m => m.name)
  if (names.length === 0) return
  const r = await put('/models/bulk', { names, enabled: false })
  toast(r.ok ? 'Disabled all models' : r.error, r.ok)
  load()
}

async function remove(m) {
  const ok = await confirmDialog(`"${m.name}" will be permanently removed.`, {
    title: 'Delete Model',
    confirmText: 'Delete',
  })
  if (!ok) return
  const r = await del(`/models/${encodeURIComponent(m.name)}`)
  toast(r.ok ? 'Deleted' : r.error, r.ok)
  load()
}

async function reset(m) {
  const r = await post(`/models/${encodeURIComponent(m.name)}/reset`, {})
  toast(r.ok ? `${m.name} reset` : r.error, r.ok)
  load()
}

// One visual state per row: disabled overrides live status.
function stateKey(m) {
  if (m.enabled === false) return 'off'
  if (m.status === 'available') return 'ok'
  if (m.status === 'rate_limited') return 'warn'
  return 'err'
}

const STATE_LABEL = { ok: 'available', warn: 'rate limited', err: 'unavailable', off: 'disabled' }

const summary = computed(() => {
  const c = { total: models.value.length, healthy: 0, rateLimited: 0, unavailable: 0, disabled: 0 }
  for (const m of models.value) {
    if (m.enabled === false) {
      c.disabled++
    } else if (m.status === 'rate_limited') {
      c.rateLimited++
    } else if (m.status === 'available') {
      c.healthy++
    } else {
      c.unavailable++
    }
  }
  return c
})

function getPct(m) {
  const rl = m.rl_snapshot || {}
  if (rl.tokens_remaining_per_min && rl.tokens_limit_per_min) {
    return Math.round((rl.tokens_remaining_per_min / rl.tokens_limit_per_min) * 100)
  }
  return null
}

onMounted(load)
</script>

<template>
  <div class="page-wrap models-page">
    <div class="page-toolbar">
      <p class="page-readout">
        <span class="readout-em">{{ summary.total }}</span> models
        · <span class="readout-em">{{ summary.healthy }}</span> healthy
        <template v-if="summary.rateLimited">
          · {{ summary.rateLimited }} rate limited
        </template>
        <template v-if="summary.unavailable">
          · {{ summary.unavailable }} unavailable
        </template>
        <template v-if="summary.disabled">
          · {{ summary.disabled }} disabled
        </template>
      </p>
      <div class="toolbar-actions">
        <button
          class="btn btn-ghost"
          @click="enableAll"
        >
          Enable all
        </button>
        <button
          class="btn btn-ghost"
          @click="disableAll"
        >
          Disable all
        </button>
        <button
          class="btn btn-ghost"
          @click="load"
        >
          Refresh
        </button>
        <button
          class="btn btn-save"
          @click="showAdd"
        >
          Add model
        </button>
      </div>
    </div>

    <div
      v-if="models.length === 0"
      class="empty-state"
    >
      <p class="empty-title">
        No models configured
      </p>
      <p class="empty-hint">
        Add a model to give the agent a brain to run on.
      </p>
    </div>

    <div
      v-else-if="filteredModels.length === 0"
      class="empty-state"
    >
      <p class="empty-title">
        No matching models
      </p>
      <p class="empty-hint">
        Nothing matches "{{ modelSearch.trim() }}". Try a different term.
      </p>
    </div>

    <section
      v-else
      class="panel"
    >
      <div class="panel-head">
        <h2 class="panel-title">
          Model fleet
        </h2>
        <span class="panel-count">{{ filteredModels.length }} shown</span>
      </div>

      <div class="row-list">
        <div
          v-for="m in filteredModels"
          :key="m.name"
          class="list-row model-row"
          :class="{ off: m.enabled === false }"
        >
          <div class="row-line">
            <div class="model-ident">
              <span
                class="state-dot"
                :class="stateKey(m)"
                :title="STATE_LABEL[stateKey(m)]"
              />
              <span class="row-title">{{ m.name }}</span>
              <span
                v-if="m.role"
                class="mono-chip"
              >{{ m.role }}</span>
              <span
                class="model-state-label"
                :class="stateKey(m)"
              >{{ STATE_LABEL[stateKey(m)] }}</span>
            </div>
            <div class="model-actions">
              <button
                class="btn btn-xs btn-ghost row-action"
                @click="showEdit(m)"
              >
                Edit
              </button>
              <button
                v-if="m.consecutive_errors > 0 || (m.status && m.status !== 'available')"
                class="btn btn-xs btn-ghost row-action"
                @click="reset(m)"
              >
                Reset
              </button>
              <button
                class="btn btn-xs btn-danger row-action"
                @click="remove(m)"
              >
                Delete
              </button>
              <button
                class="switch"
                type="button"
                role="switch"
                :aria-checked="m.enabled === false ? 'false' : 'true'"
                :aria-label="`${m.enabled === false ? 'Enable' : 'Disable'} ${m.name}`"
                :title="m.enabled === false ? 'Enable' : 'Disable'"
                @click="toggle(m)"
              />
            </div>
          </div>

          <div class="model-meta">
            <span class="mono-chip">{{ m.provider }}</span>
            <span class="mono-chip">P{{ m.priority }}</span>
            <span class="model-id">{{ m.model_id }}</span>
            <span class="model-readout">
              {{ m.total_calls }} calls
              · {{ fmtTokens(m.total_input_tokens) }} in / {{ fmtTokens(m.total_output_tokens) }} out
              · <span :class="{ 'readout-err': m.consecutive_errors > 0 }">{{ m.consecutive_errors }} errors</span>
              <template v-if="m.rate_limit_reset_at">
                · resets {{ timeAgo(m.rate_limit_reset_at) }}
              </template>
            </span>
          </div>

          <div
            v-if="getPct(m) !== null"
            class="rate-row"
            :title="`${m.rl_snapshot.tokens_remaining_per_min} / ${m.rl_snapshot.tokens_limit_per_min} tokens remaining`"
          >
            <div class="rate-track">
              <div
                class="rate-fill"
                :style="{ width: getPct(m) + '%' }"
              />
            </div>
            <span class="rate-pct">{{ getPct(m) }}% left</span>
          </div>
        </div>
      </div>
    </section>
  </div>

  <Modal
    v-model="modalOpen"
    :title="editing ? `Edit Model: ${form.name}` : 'Add AI Model'"
    :max-width="'720px'"
  >
    <div class="model-form">
      <div class="form-grid">
        <div class="form-field">
          <label>Internal name</label>
          <input
            v-model="form.name"
            type="text"
            :disabled="editing"
            placeholder="e.g. gpt-4-prod"
          >
        </div>
        <div class="form-field">
          <label>Provider</label>
          <select v-model="form.provider">
            <option
              v-for="p in PROVIDERS"
              :key="p.value"
              :value="p.value"
            >
              {{ p.label }}
            </option>
          </select>
        </div>
      </div>

      <div class="form-field">
        <label>
          Model ID
          <span
            v-if="modelsLoading"
            class="field-note"
          >loading…</span>
          <span
            v-else-if="availableModels.length"
            class="field-note"
          >{{ availableModels.length }} available</span>
        </label>
        <SearchableSelect
          v-model="form.model_id"
          :options="modelIdOptions"
          :allow-custom-value="true"
          placeholder="e.g. gpt-4o"
        />
        <p class="field-hint">
          Pick from the provider's available models, or type any ID to override.
        </p>
      </div>

      <div class="form-field">
        <label>API key</label>
        <input
          v-model="form.api_key"
          type="password"
          placeholder="••••••••••••••••"
        >
      </div>

      <div class="form-field">
        <label>Base URL (optional)</label>
        <input
          v-model="form.base_url"
          type="text"
          placeholder="https://api.openai.com/v1"
        >
      </div>

      <div class="form-grid">
        <div class="form-field">
          <label>Priority</label>
          <input
            v-model="form.priority"
            type="number"
          >
        </div>
        <div class="form-field">
          <label>Role</label>
          <select v-model="form.role">
            <option value="">
              General
            </option>
            <optgroup label="Specialists">
              <option value="router">
                Router (cron / tool select)
              </option>
              <option value="tool_writer">
                Tool Writer
              </option>
              <option value="quality_checker">
                Quality Checker
              </option>
              <option value="memory_compressor">
                Memory Compressor
              </option>
              <option value="watcher">
                Watcher
              </option>
              <option value="image_model">
                Image Model (vision / generation)
              </option>
            </optgroup>
            <option value="paid_model">
              Paid Fallback
            </option>
          </select>
        </div>
      </div>

      <div class="form-field">
        <label>Max tokens</label>
        <input
          v-model="form.max_tokens"
          type="number"
        >
      </div>
    </div>

    <div class="modal-actions">
      <button
        class="btn btn-ghost"
        @click="modalOpen = false"
      >
        Cancel
      </button>
      <button
        class="btn btn-save"
        @click="save"
      >
        Save model
      </button>
    </div>
  </Modal>
</template>

<style scoped>
.models-page {
  padding-bottom: 60px;
}

.toolbar-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

/* ── Row identity ─────────────────────────────────────────────────────────── */
.model-ident {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  flex-wrap: wrap;
}

.state-dot {
  width: 7px;
  height: 7px;
  border-radius: 999px;
  flex-shrink: 0;
}

.state-dot.ok { background: var(--green); }
.state-dot.warn { background: var(--yellow); }
.state-dot.err { background: var(--red); }
.state-dot.off { background: var(--muted); opacity: 0.5; }

.model-state-label {
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}

.model-state-label.warn { color: var(--yellow); }
.model-state-label.err { color: var(--red); }

/* ── Row actions: quiet until the row is engaged ──────────────────────────── */
.model-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.row-action {
  opacity: 0.25;
  transition: opacity 0.15s ease;
}

.model-row:hover .row-action,
.row-action:focus-visible {
  opacity: 1;
}

@media (hover: none) {
  .row-action {
    opacity: 1;
  }
}

/* ── Meta line ────────────────────────────────────────────────────────────── */
.model-meta {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px 8px;
  margin-top: 7px;
}

.model-id {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.model-readout {
  font-family: var(--font-mono);
  font-size: 0.66rem;
  color: var(--muted);
}

.readout-err {
  color: var(--red);
}

/* ── Rate-limit meter: hairline track, signal fill ────────────────────────── */
.rate-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 8px;
}

.rate-track {
  flex: 0 1 180px;
  height: 3px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text) 10%, transparent);
  overflow: hidden;
}

.rate-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--accent);
  transition: width 0.4s ease;
}

.rate-pct {
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
  white-space: nowrap;
}

/* A disabled model fades back into the membrane. */
.model-row.off .row-title {
  color: var(--muted);
}

.model-row.off .model-meta,
.model-row.off .rate-row {
  opacity: 0.55;
}

/* ── Modal form ───────────────────────────────────────────────────────────── */
.model-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: calc(100vh - 250px);
  overflow-y: auto;
  padding-right: 4px;
}

.form-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.form-field label {
  margin-top: 0;
}

.form-field input,
.form-field select {
  margin-bottom: 0;
}

/* Model-ID dropdown affordances: a count/loading note by the label and a quiet
   hint that manual entry always wins. */
.field-note {
  margin-left: 8px;
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.03em;
  text-transform: uppercase;
  color: var(--muted);
}

.field-hint {
  margin: 5px 0 0;
  font-size: 0.68rem;
  color: var(--muted);
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}

@media (max-width: 700px) {
  .form-grid {
    grid-template-columns: 1fr;
  }

  .model-actions {
    flex-wrap: wrap;
  }
}
</style>
