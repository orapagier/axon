<script setup>
import { ref, computed, onMounted } from 'vue'
import { get, post, put, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { fmtTokens, timeAgo } from '../lib/utils.js'
import Modal from '../components/Modal.vue'
import Pill from '../components/Pill.vue'

const models = ref([])
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
  modalOpen.value = true
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
  modalOpen.value = true
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

function statusType(s) {
  return s === 'available' ? 'ok' : s === 'rate_limited' ? 'warn' : 'err'
}

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
  <div class="services-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>Models</h1>
        <p class="page-desc">Manage your AI providers and monitor rate limits.</p>
      </div>
      <div class="header-actions">
        <button class="btn btn-ghost" @click="enableAll">⚡ Enable All</button>
        <button class="btn btn-ghost" @click="disableAll">⏸ Disable All</button>
        <button class="btn btn-save" @click="showAdd">+ Add Model</button>
        <button class="btn btn-ghost" @click="load">↻ Refresh</button>
      </div>
    </div>

    <div v-if="models.length === 0" class="empty-state-container">
      <div class="empty-state">No models configured. Add one to get started.</div>
    </div>
    
    <div v-else class="models-list">
      <div class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <h2>Active Models</h2>
          </div>
          <div class="fleet-summary">
            <span class="fleet-total">{{ summary.total }} configured</span>
            <span class="fleet-stat" :class="{ dim: summary.healthy === 0 }">
              <i class="dot ok"></i>{{ summary.healthy }} healthy
            </span>
            <span class="fleet-stat" :class="{ dim: summary.rateLimited === 0 }">
              <i class="dot warn"></i>{{ summary.rateLimited }} rate limited
            </span>
            <span class="fleet-stat" :class="{ dim: summary.unavailable === 0 }">
              <i class="dot err"></i>{{ summary.unavailable }} unavailable
            </span>
            <span class="fleet-stat" :class="{ dim: summary.disabled === 0 }">
              <i class="dot muted"></i>{{ summary.disabled }} disabled
            </span>
          </div>
        </div>

        <div class="service-list">
          <div
            v-for="m in models"
            :key="m.name"
            class="service-item model-row"
            :class="{ disabled: m.enabled === false }"
          >
            <div class="service-info">
              <div class="service-name-row">
                <div class="service-name-group">
                  <span class="service-name">{{ m.name }}</span>
                  <Pill v-if="m.enabled === false" type="muted" text="Disabled" />
                  <Pill :type="statusType(m.status)" :text="m.status" />
                  <template v-if="m.role">
                    <Pill type="info" :text="m.role.toUpperCase()" />
                  </template>
                </div>
                <div class="service-actions">
                  <button class="btn btn-sm btn-ghost" @click="showEdit(m)">Edit</button>
                  <button
                    v-if="m.consecutive_errors > 0 || (m.status && m.status !== 'available')"
                    class="btn btn-sm btn-ghost"
                    @click="reset(m)"
                  >
                    Reset
                  </button>
                  <button class="btn btn-sm btn-ghost" @click="toggle(m)">
                    {{ m.enabled === false ? 'Enable' : 'Disable' }}
                  </button>
                  <button class="btn btn-sm btn-danger" @click="remove(m)">✕</button>
                </div>
              </div>

              <div class="model-meta-line">
                <span class="provider-pill">{{ m.provider }}</span>
                <span class="model-id-text">{{ m.model_id }}</span>
                <span class="priority-pill">P{{ m.priority }}</span>
                <span class="meta-sep">·</span>
                <span>{{ m.total_calls }} calls</span>
                <span class="meta-sep">·</span>
                <span>{{ fmtTokens(m.total_input_tokens) }} in / {{ fmtTokens(m.total_output_tokens) }} out</span>
                <span class="meta-sep">·</span>
                <span :class="{ 'text-danger': m.consecutive_errors > 0 }">{{ m.consecutive_errors }} errors</span>
                <template v-if="m.rate_limit_reset_at">
                  <span class="meta-sep">·</span>
                  <span>resets {{ timeAgo(m.rate_limit_reset_at) }}</span>
                </template>
              </div>

              <div v-if="getPct(m) !== null" class="rate-limit-row">
                <div class="premium-progress" :title="`${m.rl_snapshot.tokens_remaining_per_min} / ${m.rl_snapshot.tokens_limit_per_min} tokens remaining`">
                  <div class="progress-fill" :style="{ width: getPct(m) + '%' }"></div>
                </div>
                <span class="rate-limit-pct">{{ getPct(m) }}% left</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>

  <Modal
    v-model="modalOpen"
    :title="editing ? `Edit Model: ${form.name}` : 'Add AI Model'"
    :max-width="'780px'"
  >
    <div class="model-modal-body">
      <div class="form-container">
      <div class="form-row-modern">
        <div class="form-group-modern flex-1">
          <label>Internal Name</label>
          <input type="text" v-model="form.name" :disabled="editing" class="premium-input" placeholder="e.g. gpt-4-prod" />
        </div>
        <div class="form-group-modern flex-1">
          <label>Provider</label>
          <select v-model="form.provider" class="premium-input select-input">
            <option v-for="p in PROVIDERS" :key="p.value" :value="p.value">{{ p.label }}</option>
          </select>
        </div>
      </div>
      <div class="form-group-modern">
        <label>Model ID</label>
        <input type="text" v-model="form.model_id" class="premium-input" placeholder="e.g. gpt-4o" />
      </div>
      <div class="form-group-modern">
        <label>API Key</label>
        <input type="password" v-model="form.api_key" class="premium-input" placeholder="••••••••••••••••" />
      </div>
      <div class="form-group-modern">
        <label>Base URL (Optional)</label>
        <input type="text" v-model="form.base_url" class="premium-input" placeholder="https://api.openai.com/v1" />
      </div>
      <div class="form-row-modern">
        <div class="form-group-modern flex-1">
          <label>Priority</label>
          <input type="number" v-model="form.priority" class="premium-input" />
        </div>
        <div class="form-group-modern flex-1">
          <label>Role</label>
          <select v-model="form.role" class="premium-input select-input">
            <option value="">General</option>
            <optgroup label="Specialists">
              <option value="router">Router (cron / tool select)</option>
              <option value="tool_writer">Tool Writer</option>
              <option value="quality_checker">Quality Checker</option>
              <option value="memory_compressor">Memory Compressor</option>
              <option value="watcher">Watcher</option>
              <option value="image_model">Image Model (vision)</option>
            </optgroup>
            <option value="paid_model">Paid Fallback</option>
          </select>
        </div>
      </div>
      <div class="form-group-modern">
        <label>Max Tokens</label>
        <input type="number" v-model="form.max_tokens" class="premium-input" />
      </div>
      </div>
    </div>
    <div class="modal-actions-modern">
      <button class="btn btn-ghost" @click="modalOpen = false">Cancel</button>
      <button class="btn btn-save" @click="save">Save Model</button>
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
  margin-bottom: 18px;
}

.page-header h1 {
  font-size: 22px;
  font-weight: 800;
  letter-spacing: -0.02em;
  margin-bottom: 4px;
  background: linear-gradient(90deg, #1e2433, #6c5ce7);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.page-desc {
  color: var(--muted);
  font-size: 13px;
  margin: 0;
}

/* Premium Cards */
.premium-card {
  background: rgba(255, 255, 255, 0.4);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(0, 0, 0, 0.05);
  border-radius: 12px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.12);
  margin-bottom: 16px;
  overflow: hidden;
  transition: all 0.3s cubic-bezier(0.16, 1, 0.3, 1);
}

.card-header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 20px;
  background: rgba(0, 0, 0, 0.1);
  flex-wrap: wrap;
  gap: 8px;
}

.fleet-summary {
  display: flex;
  align-items: center;
  gap: 14px;
  flex-wrap: wrap;
}

.fleet-total {
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
}

.fleet-stat {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
  white-space: nowrap;
}

.fleet-stat.dim {
  opacity: 0.45;
}

.dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  display: inline-block;
  flex-shrink: 0;
}

.dot.ok { background: #00cec9; }
.dot.warn { background: #f59e0b; }
.dot.err { background: #ff7675; }
.dot.muted { background: var(--muted); }

.card-title-group h2 {
  font-size: 13px;
  font-weight: 800;
  color: var(--text);
  letter-spacing: 0.1em;
  text-transform: uppercase;
  margin: 0;
}

.service-list {
  display: flex;
  flex-direction: column;
}

.service-item {
  padding: 12px 20px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.05);
  transition: background 0.2s;
}

.service-item:hover {
  background: rgba(0, 0, 0, 0.02);
}

.service-item:last-child {
  border-bottom: none;
}

.service-item.disabled {
  opacity: 0.6;
}

.service-info {
  width: 100%;
}

.service-name-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 6px;
  gap: 12px;
  flex-wrap: wrap;
}

.service-name-group {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.service-name {
  font-size: 14px;
  font-weight: 700;
  color: var(--text);
}

.model-meta-line {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--muted);
}

.meta-sep {
  opacity: 0.4;
}

.provider-pill {
  font-size: 10px;
  font-weight: 800;
  text-transform: uppercase;
  color: #a29bfe;
  background: rgba(162, 155, 254, 0.1);
  padding: 1px 7px;
  border-radius: 4px;
}

.model-id-text {
  font-size: 12px;
  color: var(--muted);
  font-family: 'Fira Code', monospace;
}

.priority-pill {
  font-size: 10px;
  font-weight: 700;
  color: var(--muted);
  background: rgba(0, 0, 0, 0.05);
  padding: 1px 6px;
  border-radius: 4px;
}

.text-danger {
  color: #ff7675;
}

/* Rate Limit Bar */
.rate-limit-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
}

.premium-progress {
  flex: 1;
  max-width: 200px;
  height: 4px;
  background: rgba(0, 0, 0, 0.05);
  border-radius: 10px;
  overflow: hidden;
}

.progress-fill {
  height: 100%;
  background: linear-gradient(90deg, #6c5ce7, #a29bfe);
  border-radius: 10px;
  transition: width 0.5s cubic-bezier(0.16, 1, 0.3, 1);
}

.rate-limit-pct {
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
}

.empty-state-container {
  padding: 60px 0;
  text-align: center;
}

.empty-state {
  color: var(--muted);
  font-style: italic;
  font-size: 15px;
}

/* Modal Modernization */
.form-container {
  display: flex;
  flex-direction: column;
  gap: 20px;
  margin-bottom: 24px;
}

.model-modal-body {
  max-height: calc(100vh - 250px);
  overflow-y: auto;
  padding-right: 4px;
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

.flex-1 { flex: 1; }

.premium-input {
  width: 100%;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 10px;
  color: var(--text);
  padding: 12px 16px;
  font-size: 14px;
  font-family: inherit;
  transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
  outline: none;
}

.premium-input::placeholder {
  color: rgba(237, 244, 247, 0.35);
}

.premium-input:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.premium-input:focus {
  background: rgba(255, 255, 255, 0.09);
  border-color: var(--teal);
  box-shadow: 0 0 0 3px rgba(129, 230, 217, 0.15);
}

.select-input {
  appearance: none;
  background-image: url("data:image/svg+xml;charset=UTF-8,%3csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='white' stroke-width='2' stroke-linecap='round' stroke-linejoin='round' opacity='0.5'%3e%3cpolyline points='6 9 12 15 18 9'%3e%3c/polyline%3e%3c/svg%3e");
  background-repeat: no-repeat;
  background-position: right 12px center;
  background-size: 16px;
  padding-right: 40px;
}

/* Dropdown options render with OS colors — force dark-theme readable values */
.select-input option {
  background: var(--surface);
  color: var(--text);
}

.modal-actions-modern {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  padding-top: 20px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
}

/* Enhanced Buttons */
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 10px 20px;
  font-size: 13px;
  font-weight: 700;
  border-radius: 10px;
  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
  cursor: pointer;
  border: 1px solid transparent;
}

.btn-sm {
  padding: 8px 16px;
  font-size: 12px;
}

.btn-save {
  background: linear-gradient(135deg, #00b894 0%, #00cec9 100%);
  color: #fff;
  box-shadow: 0 4px 15px rgba(0, 206, 201, 0.2);
}

.btn-save:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 25px rgba(0, 206, 201, 0.3);
}

.btn-primary {
  background: linear-gradient(135deg, #6c5ce7 0%, #a29bfe 100%);
  color: #fff;
}

.btn-ghost {
  background: rgba(0, 0, 0, 0.05);
  border: 1px solid rgba(0, 0, 0, 0.1);
  color: var(--text);
}

.btn-ghost:hover {
  background: rgba(0, 0, 0, 0.1);
}

.btn-danger {
  background: rgba(244, 63, 94, 0.1);
  color: #fb7185;
  border: 1px solid rgba(244, 63, 94, 0.2);
}

.btn-danger:hover {
  background: rgba(244, 63, 94, 0.2);
  color: var(--text);
}

@media (max-width: 768px) {
  .form-row-modern {
    flex-direction: column;
    gap: 20px;
  }

  .model-modal-body {
    max-height: calc(100vh - 220px);
  }
}
</style>
