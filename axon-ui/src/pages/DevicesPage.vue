<script setup>
import { ref, onMounted, onUnmounted } from 'vue'
import { get, post, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import Modal from '../components/Modal.vue'

const GLYPH_DEVICE = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="7" y="2" width="10" height="20" rx="2"/><path d="M11 18h2"/></svg>`

const devices = ref([])
const deviceModal = ref(false)
const editingDevice = ref(false)
const deviceForm = ref({ name: '', kind: 'androidcompanion', base_url: '', bearer_token: '', notes: '' })

const testing = ref({})
const testResults = ref({})
const expandedDevice = ref(null)
const toolsLoading = ref({})
const deviceTools = ref({})

// Background reachability poll so the connected/offline dot on each tile stays
// live without the user having to click "Test connection" — the server itself
// dedupes repeated failures so this doesn't spam the notification bell/messaging
// gateway, it only reports on a connected<->disconnected transition.
const POLL_INTERVAL_MS = 30000
let pollTimer = null

async function loadDevices() {
  const d = await get('/devices')
  devices.value = d.devices || []
}

async function testAllDevices() {
  await Promise.all(devices.value.map((d) => testDevice(d.name)))
}

function connState(name) {
  const r = testResults.value[name]
  if (!r) return 'checking'
  return r.ok ? 'connected' : 'offline'
}

function connLabel(name) {
  const state = connState(name)
  if (state === 'connected') return 'Connected'
  if (state === 'offline') return 'Disconnected'
  return 'Checking…'
}

function showAddDevice() {
  editingDevice.value = false
  deviceForm.value = { name: '', kind: 'androidcompanion', base_url: '', bearer_token: '', notes: '' }
  deviceModal.value = true
}

function showEditDevice(d) {
  editingDevice.value = true
  deviceForm.value = {
    name: d.name,
    kind: d.kind,
    base_url: d.base_url,
    bearer_token: '',
    notes: d.notes || '',
  }
  deviceModal.value = true
}

async function saveDevice() {
  if (!deviceForm.value.name || !deviceForm.value.base_url) {
    return toast('Name and Base URL are required', false)
  }
  const r = await post('/devices', deviceForm.value)
  toast(r.ok ? 'Device saved' : r.error, r.ok)
  if (r.ok) {
    deviceModal.value = false
    const name = deviceForm.value.name
    await loadDevices()
    testDevice(name)
  }
}

async function deleteDevice(name) {
  const ok = await confirmDialog(`"${name}" will be permanently removed from your devices.`, {
    title: 'Remove Device',
    confirmText: 'Remove',
  })
  if (!ok) return
  await del(`/devices/${encodeURIComponent(name)}`)
  if (expandedDevice.value === name) expandedDevice.value = null
  loadDevices()
}

async function testDevice(name) {
  testing.value = { ...testing.value, [name]: true }
  const r = await post(`/devices/${encodeURIComponent(name)}/test`)
  testResults.value = { ...testResults.value, [name]: r }
  testing.value = { ...testing.value, [name]: false }
}

async function toggleEndpoints(name) {
  if (expandedDevice.value === name) {
    expandedDevice.value = null
    return
  }
  expandedDevice.value = name
  if (!deviceTools.value[name]) {
    toolsLoading.value = { ...toolsLoading.value, [name]: true }
    const r = await get(`/devices/${encodeURIComponent(name)}/tools`)
    deviceTools.value = { ...deviceTools.value, [name]: r }
    toolsLoading.value = { ...toolsLoading.value, [name]: false }
  }
}

function endpointsFor(name) {
  const r = deviceTools.value[name]
  return r?.body?.tools || r?.tools || []
}

onMounted(async () => {
  await loadDevices()
  testAllDevices()
  pollTimer = setInterval(testAllDevices, POLL_INTERVAL_MS)
})

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer)
})
</script>

<template>
  <div class="page-wrap devices-page">
    <section class="svc-group">
      <header class="svc-group-head">
        <h2 class="svc-group-title">
          Companion Devices
        </h2>
        <span class="svc-group-count">{{ devices.length }} registered</span>
      </header>

      <div class="svc-grid">
        <article
          v-for="d in devices"
          :key="d.name"
          class="svc-tile"
        >
          <div class="svc-tile-top">
            <span
              class="svc-icon glyph"
              v-html="GLYPH_DEVICE"
            />
            <span class="svc-name">{{ d.name }}</span>
            <span
              class="conn-indicator"
              :class="connState(d.name)"
              :title="connLabel(d.name)"
            >
              <span class="conn-dot" />{{ connLabel(d.name) }}
            </span>
            <span class="svc-status warm">{{ d.kind }}</span>
          </div>
          <p class="svc-meta mono">
            {{ d.base_url }}
          </p>
          <p
            v-if="d.notes"
            class="svc-meta"
          >
            {{ d.notes }}
          </p>

          <div
            v-if="expandedDevice === d.name"
            class="svc-endpoints"
          >
            <p
              v-if="toolsLoading[d.name]"
              class="svc-meta"
            >
              Loading endpoints…
            </p>
            <p
              v-else-if="deviceTools[d.name]?.error"
              class="svc-meta svc-test-err"
            >
              {{ deviceTools[d.name].error }}
            </p>
            <ul
              v-else
              class="svc-endpoint-list"
            >
              <li
                v-for="t in endpointsFor(d.name)"
                :key="t.name"
              >
                <code>{{ t.name }}</code>
                <span class="svc-meta"> — {{ t.description }}</span>
                <span
                  v-if="t.params?.length"
                  class="svc-meta mono"
                > ({{ t.params.join(', ') }})</span>
              </li>
              <li v-if="!endpointsFor(d.name).length">
                <span class="svc-meta">No endpoints returned.</span>
              </li>
            </ul>
          </div>

          <div class="svc-actions">
            <button
              class="btn btn-ghost"
              :disabled="testing[d.name]"
              @click="testDevice(d.name)"
            >
              {{ testing[d.name] ? 'Testing…' : 'Test connection' }}
            </button>
            <button
              class="btn btn-ghost"
              @click="toggleEndpoints(d.name)"
            >
              {{ expandedDevice === d.name ? 'Hide endpoints' : 'View endpoints' }}
            </button>
            <button
              class="btn btn-ghost"
              @click="showEditDevice(d)"
            >
              Edit
            </button>
            <button
              class="btn btn-ghost svc-danger"
              @click="deleteDevice(d.name)"
            >
              Remove
            </button>
          </div>
        </article>

        <button
          class="svc-add-tile"
          type="button"
          @click="showAddDevice"
        >
          + Add device
        </button>
      </div>
    </section>

    <Modal
      v-model="deviceModal"
      :title="editingDevice ? 'Edit Device' : 'Add Device'"
    >
      <div class="mform">
        <div class="mform-field">
          <label>Device Name</label>
          <input
            v-model="deviceForm.name"
            type="text"
            placeholder="e.g. Samsung Galaxy Note 1"
          >
        </div>

        <div class="mform-field">
          <label>Base URL</label>
          <input
            v-model="deviceForm.base_url"
            type="text"
            placeholder="https://phone.yourdomain.com"
          >
        </div>

        <div class="mform-row">
          <div class="mform-field grow-1">
            <label>Kind</label>
            <select v-model="deviceForm.kind">
              <option value="androidcompanion">
                AndroidCompanion
              </option>
              <option value="windowscompanion">
                Windows Companion
              </option>
            </select>
          </div>
        </div>

        <div class="mform-field">
          <label>Bearer Token</label>
          <input
            v-model="deviceForm.bearer_token"
            type="password"
            placeholder="••••••••••••••••"
          >
          <p
            v-if="editingDevice"
            class="mform-hint"
          >
            Leave blank to keep the existing token.
          </p>
        </div>

        <div class="mform-field">
          <label>Notes (optional)</label>
          <textarea
            v-model="deviceForm.notes"
            rows="2"
          />
        </div>

        <div class="mform-actions">
          <button
            class="btn btn-ghost"
            @click="deviceModal = false"
          >
            Cancel
          </button>
          <button
            class="btn btn-save"
            @click="saveDevice"
          >
            Save
          </button>
        </div>
      </div>
    </Modal>
  </div>
</template>

<style scoped>
.devices-page {
  padding-bottom: 60px;
}

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

.svc-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
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
  transition: border-color 0.15s var(--ease-out);
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

.svc-status.warm {
  color: var(--teal);
}

.conn-indicator {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
  font-family: var(--font-mono);
  font-size: 0.62rem;
  letter-spacing: 0.02em;
}

.conn-dot {
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: currentColor;
  opacity: 0.9;
}

.conn-indicator.connected {
  color: var(--green);
}

.conn-indicator.offline {
  color: var(--red);
}

.conn-indicator.checking {
  color: var(--muted);
}

.svc-meta {
  margin: 0;
  min-height: 0;
  font-size: 0.72rem;
  line-height: 1.5;
  color: var(--muted);
  overflow-wrap: anywhere;
}

.svc-meta.mono {
  font-family: var(--font-mono);
  font-size: 0.68rem;
}

.svc-test-err {
  color: var(--red);
}

.svc-endpoints {
  padding: 8px 0;
  border-top: 1px dashed var(--border);
}

.svc-endpoint-list {
  margin: 0;
  padding: 0;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.svc-endpoint-list code {
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--text);
}

.svc-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: auto;
}

.svc-danger:hover {
  border-color: color-mix(in srgb, var(--red) 50%, transparent) !important;
  color: var(--red) !important;
}

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
  transition: border-color 0.15s var(--ease-out), color 0.15s var(--ease-out), background 0.15s var(--ease-out);
}

.svc-add-tile:hover {
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 4%, transparent);
}

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

.mform-row {
  display: flex;
  gap: 12px;
}

.grow-1 {
  flex: 1;
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
}
</style>
