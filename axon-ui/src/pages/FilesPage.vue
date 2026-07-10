<script setup>
import { ref, onMounted, computed } from 'vue'
import { get, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { timeAgo, fmtBytes } from '../lib/utils.js'
import { useHeaderSearch } from '../lib/headerSearch.js'

function getFileExt(name) {
  if (!name) return '?'
  const p = name.split('.')
  return p.length > 1 ? p[p.length - 1].slice(0, 4) : 'file'
}

const incoming = ref([])
const outgoing = ref([])
const masterKey = localStorage.getItem('AXON_MASTER_KEY') || ''
const hasFiles = computed(() => incoming.value.length > 0 || outgoing.value.length > 0)

const searchQuery = ref('')

useHeaderSearch('files', {
  query: searchQuery,
  placeholder: 'Search files by name…',
})
function byFilename(list) {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return list
  return list.filter((f) => (f.filename || '').toLowerCase().includes(q))
}
const filteredIncoming = computed(() => byFilename(incoming.value))
const filteredOutgoing = computed(() => byFilename(outgoing.value))

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
  const ok = await confirmDialog('This file will be permanently deleted. This action cannot be undone.', {
    title: 'Delete File',
    confirmText: 'Delete',
  })
  if (!ok) return
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
  const ok = await confirmDialog(`All ${allFilesCount} files will be permanently deleted. This action cannot be undone.`, {
    title: 'Delete All Files',
    confirmText: 'Delete All',
  })
  if (!ok) return

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
  <div class="page-wrap files-page">
    <div class="page-toolbar">
      <p class="page-readout">
        <span class="readout-em">{{ incoming.length }}</span> incoming
        · <span class="readout-em">{{ outgoing.length }}</span> outgoing
      </p>
      <div class="toolbar-actions">
        <button
          class="btn btn-danger"
          :disabled="!hasFiles"
          @click="removeAll"
        >
          Delete all
        </button>
        <button
          class="btn btn-ghost"
          @click="load"
        >
          Refresh
        </button>
        <label class="btn btn-save file-upload-label">
          <span>Upload file</span>
          <input
            type="file"
            hidden
            @change="upload"
          >
        </label>
      </div>
    </div>

    <div class="files-grid">
      <section class="panel">
        <div class="panel-head">
          <h2 class="panel-title">
            Incoming
          </h2>
          <span class="panel-count">{{ filteredIncoming.length }} files</span>
        </div>

        <div
          v-if="filteredIncoming.length === 0"
          class="panel-empty"
        >
          {{ searchQuery.trim() ? 'No incoming files match your search.' : 'No incoming files.' }}
        </div>
        <div
          v-else
          class="row-list"
        >
          <div
            v-for="f in filteredIncoming"
            :key="f.id"
            class="list-row file-row"
          >
            <div class="row-line">
              <div class="file-ident">
                <span class="mono-chip file-ext">{{ getFileExt(f.filename) }}</span>
                <span class="file-name">{{ f.filename }}</span>
              </div>
              <div class="file-actions">
                <a
                  class="btn btn-xs btn-ghost btn-icon row-action"
                  :href="`/api/download?path=${encodeURIComponent(f.path)}&api_key=${encodeURIComponent(masterKey)}`"
                  :download="f.filename"
                  title="Download file"
                  aria-label="Download file"
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
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
                  class="btn btn-xs btn-ghost btn-icon text-error row-action"
                  title="Delete file"
                  aria-label="Delete file"
                  @click="remove(f.direction || 'incoming', f.id)"
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
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
            <p class="file-readout">
              {{ f.mime_type || 'unknown type' }} · {{ fmtBytes(f.size_bytes) }} · {{ timeAgo(f.created_at) }}
            </p>
          </div>
        </div>
      </section>

      <section class="panel">
        <div class="panel-head">
          <h2 class="panel-title">
            Outgoing
          </h2>
          <span class="panel-count">{{ filteredOutgoing.length }} files</span>
        </div>

        <div
          v-if="filteredOutgoing.length === 0"
          class="panel-empty"
        >
          {{ searchQuery.trim() ? 'No outgoing files match your search.' : 'No outgoing files.' }}
        </div>
        <div
          v-else
          class="row-list"
        >
          <div
            v-for="f in filteredOutgoing"
            :key="f.id"
            class="list-row file-row"
          >
            <div class="row-line">
              <div class="file-ident">
                <span class="mono-chip file-ext">{{ getFileExt(f.filename) }}</span>
                <span class="file-name">{{ f.filename }}</span>
              </div>
              <div class="file-actions">
                <a
                  class="btn btn-xs btn-ghost btn-icon row-action"
                  :href="`/api/download?path=${encodeURIComponent(f.path)}&api_key=${encodeURIComponent(masterKey)}`"
                  :download="f.filename"
                  title="Download file"
                  aria-label="Download file"
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
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
                  class="btn btn-xs btn-ghost btn-icon text-error row-action"
                  title="Delete file"
                  aria-label="Delete file"
                  @click="remove(f.direction || 'outgoing', f.id)"
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="14"
                    height="14"
                    aria-hidden="true"
                  >
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
            <p class="file-readout">
              {{ f.mime_type || 'unknown type' }} · {{ fmtBytes(f.size_bytes) }} · {{ timeAgo(f.created_at) }}
            </p>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.files-page {
  padding-bottom: 60px;
}

.toolbar-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.files-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 14px;
  align-items: start;
}

.panel-empty {
  padding: 32px 16px;
  text-align: center;
  font-family: var(--font-mono);
  font-size: 0.7rem;
  color: var(--muted);
}

.file-ident {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  flex: 1;
}

.file-ext {
  flex-shrink: 0;
  text-transform: uppercase;
}

.file-name {
  font-size: 0.8rem;
  font-weight: 600;
  color: var(--text);
  overflow-wrap: anywhere;
}

.file-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.row-action {
  opacity: 0.25;
  transition: opacity 0.15s ease;
}

.file-row:hover .row-action,
.row-action:focus-visible {
  opacity: 1;
}

@media (hover: none) {
  .row-action {
    opacity: 1;
  }
}

.file-readout {
  margin: 6px 0 0;
  font-family: var(--font-mono);
  font-size: 0.64rem;
  color: var(--muted);
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
