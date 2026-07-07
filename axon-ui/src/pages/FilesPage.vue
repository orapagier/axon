<script setup>
import { ref, onMounted, computed } from 'vue'
import { get, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { timeAgo, fmtBytes } from '../lib/utils.js'

function getFileExt(name) {
  if (!name) return '?'
  const p = name.split('.')
  return p.length > 1 ? p[p.length - 1].slice(0, 4) : 'file'
}

const incoming = ref([])
const outgoing = ref([])
const masterKey = localStorage.getItem('AXON_MASTER_KEY') || ''
const hasFiles = computed(() => incoming.value.length > 0 || outgoing.value.length > 0)

async function load() {
  try {
    const [inc, out] = await Promise.all([get('/files/incoming'), get('/files/outgoing')])
    incoming.value = inc.files || []
    outgoing.value = out.files || []
  } catch (err) {
    console.error('Failed to load files:', err)
  }
}

async function upload(e) {
  const file = e.target.files[0]
  if (!file) return
  const form = new FormData()
  form.append('file', file)
  const storedMasterKey = localStorage.getItem('AXON_MASTER_KEY')
  const headers = {}
  if (storedMasterKey) headers.Authorization = `Bearer ${storedMasterKey}`

  try {
    const res = await fetch('/api/upload', {
      method: 'POST',
      headers,
      body: form,
    })

    if (!res.ok) {
      const txt = await res.text()
      throw new Error(txt || `Server error ${res.status}`)
    }

    const r = await res.json()
    toast(r.ok ? `Uploaded: ${file.name}` : r.error, r.ok)
    load()
    e.target.value = ''
  } catch (err) {
    toast(`Upload failed: ${err.message}`, false)
    e.target.value = ''
  }
}

async function remove(dir, id) {
  if (!confirm('Delete this file?')) return
  try {
    const r = await del(`/files/${dir}/${id}`)
    toast(r.ok ? 'File deleted' : r.error, r.ok)
    load()
  } catch (err) {
    toast(`Delete failed: ${err.message}`, false)
  }
}

async function removeAll() {
  const allFilesCount = incoming.value.length + outgoing.value.length

  if (allFilesCount === 0) return
  if (!confirm(`Delete all ${allFilesCount} files?`)) return

  try {
    const r = await del('/files/delete-all')
    toast(r.ok ? `Deleted ${r.deleted ?? 0} files` : r.error, r.ok)
  } catch (err) {
    toast(`Delete all failed: ${err.message}`, false)
  }

  load()
}

onMounted(load)
</script>

<template>
  <div class="services-page files-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>Files</h1>
        <p class="page-desc">Browse and manage files shared between you and the AI agents.</p>
      </div>
      <div class="header-actions">
        <label class="btn btn-save file-upload-label">
          <span>+ Upload File</span>
          <input type="file" hidden @change="upload" />
        </label>
        <button class="btn btn-danger" @click="removeAll" :disabled="!hasFiles">Delete All</button>
        <button class="btn btn-ghost" @click="load">Refresh</button>
      </div>
    </div>

    <div class="files-grid">
      <section class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <div class="card-icon incoming-icon">IN</div>
            <h2>Incoming Files</h2>
          </div>
          <span class="card-summary">{{ incoming.length }} files</span>
        </div>

        <div class="card-content">
          <div v-if="incoming.length === 0" class="empty-state">No incoming files found.</div>
          <div v-else class="service-list">
            <div v-for="f in incoming" :key="f.id" class="service-item file-row">
              <div class="file-type-icon">{{ getFileExt(f.filename) }}</div>
              <div class="service-info">
                <div class="service-name-row">
                  <div class="service-name-group">
                    <span class="service-name">{{ f.filename }}</span>
                  </div>
                  <div class="service-actions">
                    <a
                      class="btn btn-sm btn-ghost icon-action-btn"
                      :href="`/api/download?path=${encodeURIComponent(f.path)}&api_key=${encodeURIComponent(masterKey)}`"
                      :download="f.filename"
                      title="Download file"
                      aria-label="Download file"
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path
                          d="M12 3v11m0 0 4-4m-4 4-4-4M5 19h14"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.8"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </a>
                    <button
                      class="btn btn-sm btn-ghost text-error icon-action-btn"
                      @click="remove(f.direction || 'incoming', f.id)"
                      title="Delete file"
                      aria-label="Delete file"
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path
                          d="M4 7h16m-11 0V5h6v2m-7 0v11m4-11v11m4-11v11M7 7l1 13h8l1-13"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.8"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </button>
                  </div>
                </div>
                <div class="file-meta-row">
                  <span class="meta-item">{{ f.mime_type || 'unknown type' }}</span>
                  <span class="divider">•</span>
                  <span class="meta-item">{{ fmtBytes(f.size_bytes) }}</span>
                  <span class="divider">•</span>
                  <span class="meta-item">{{ timeAgo(f.created_at) }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      <section class="premium-card">
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <div class="card-icon outgoing-icon">OUT</div>
            <h2>Outgoing Files</h2>
          </div>
          <span class="card-summary">{{ outgoing.length }} files</span>
        </div>

        <div class="card-content">
          <div v-if="outgoing.length === 0" class="empty-state">No outgoing files found.</div>
          <div v-else class="service-list">
            <div v-for="f in outgoing" :key="f.id" class="service-item file-row">
              <div class="file-type-icon outgoing">{{ getFileExt(f.filename) }}</div>
              <div class="service-info">
                <div class="service-name-row">
                  <div class="service-name-group">
                    <span class="service-name">{{ f.filename }}</span>
                  </div>
                  <div class="service-actions">
                    <a
                      class="btn btn-sm btn-ghost icon-action-btn"
                      :href="`/api/download?path=${encodeURIComponent(f.path)}&api_key=${encodeURIComponent(masterKey)}`"
                      :download="f.filename"
                      title="Download file"
                      aria-label="Download file"
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path
                          d="M12 3v11m0 0 4-4m-4 4-4-4M5 19h14"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.8"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </a>
                    <button
                      class="btn btn-sm btn-ghost text-error icon-action-btn"
                      @click="remove(f.direction || 'outgoing', f.id)"
                      title="Delete file"
                      aria-label="Delete file"
                    >
                      <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path
                          d="M4 7h16m-11 0V5h6v2m-7 0v11m4-11v11m4-11v11M7 7l1 13h8l1-13"
                          fill="none"
                          stroke="currentColor"
                          stroke-width="1.8"
                          stroke-linecap="round"
                          stroke-linejoin="round"
                        />
                      </svg>
                    </button>
                  </div>
                </div>
                <div class="file-meta-row">
                  <span class="meta-item">{{ f.mime_type || 'unknown type' }}</span>
                  <span class="divider">•</span>
                  <span class="meta-item">{{ fmtBytes(f.size_bytes) }}</span>
                  <span class="divider">•</span>
                  <span class="meta-item">{{ timeAgo(f.created_at) }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.files-page {
  padding-bottom: 40px;
}

.files-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 18px;
  align-items: start;
}

.premium-card {
  overflow: hidden;
  height: 100%;
}

.card-header-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.card-title-group {
  display: flex;
  align-items: center;
  gap: 10px;
}

.card-title-group h2 {
  margin: 0;
}

.card-icon {
  width: 28px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.08em;
}

.incoming-icon {
  background: rgba(183, 215, 154, 0.12);
  color: #5a7d2a;
}

.outgoing-icon {
  background: rgba(184, 204, 199, 0.12);
  color: #b7ccc7;
}

.service-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.file-row {
  display: flex;
  align-items: flex-start;
  gap: 14px;
}

.file-type-icon {
  width: 40px;
  height: 40px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 12px;
  background: rgba(0, 0, 0, 0.04);
  border: 1px solid rgba(0, 0, 0, 0.06);
  font-size: 10px;
  font-weight: 800;
  text-transform: uppercase;
  color: rgba(244, 242, 237, 0.62);
}

.file-type-icon.outgoing {
  color: #b7ccc7;
}

.service-info {
  flex: 1;
  min-width: 0;
}

.service-name-row {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 6px;
}

.service-name {
  font-size: 0.92rem;
  font-weight: 600;
  color: #23272e;
}

.file-meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.meta-item {
  font-size: 0.74rem;
  color: #97a59f;
}

.divider {
  color: var(--muted);
}

.empty-state {
  padding: 36px 20px;
  text-align: center;
  color: #97a59f;
  font-size: 0.82rem;
}

.text-error {
  color: #b14a4a !important;
}

.icon-action-btn {
  width: 32px;
  min-width: 32px;
  height: 32px;
  padding: 0 !important;
}

.icon-action-btn svg {
  width: 15px;
  height: 15px;
}

.file-upload-label span {
  pointer-events: none;
}

@media (max-width: 960px) {
  .files-grid {
    grid-template-columns: 1fr;
  }
}
</style>
