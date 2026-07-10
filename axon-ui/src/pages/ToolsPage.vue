<script setup>
import { computed, onMounted, ref } from 'vue'
import { get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import Pill from '../components/Pill.vue'
import SearchInput from '../components/SearchInput.vue'

const bySource = ref({})
const searchQuery = ref('')

const filteredBySource = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return bySource.value
  const out = {}
  for (const [key, tools] of Object.entries(bySource.value)) {
    const matches = tools.filter(
      (t) => t.name.toLowerCase().includes(q) || (t.description || '').toLowerCase().includes(q)
    )
    if (matches.length) out[key] = matches
  }
  return out
})

const sections = computed(() =>
  Object.entries(filteredBySource.value)
    .map(([key, tools]) => ({
      key,
      title: `${String(key).toUpperCase()} TOOLS`,
      count: tools.length,
      tools,
    }))
    .sort((a, b) => a.title.localeCompare(b.title))
)

async function load() {
  const d = await get('/tools')
  const tools = d.tools || []
  const grouped = {}
  tools.forEach((t) => {
    const src = t.source?.source_type || 'internal'
    ;(grouped[src] = grouped[src] || []).push(t)
  })
  bySource.value = grouped
}

async function reload() {
  const r = await post('/tools/reload', { dir: 'tools' })
  toast(r.ok ? `${r.count} tools loaded` : r.error, r.ok)
  load()
}

async function toggleTool(t) {
  await put(`/tools/${encodeURIComponent(t.name)}`, { enabled: !t.enabled })
  load()
}

onMounted(load)
</script>

<template>
  <div class="services-page tools-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>Tools</h1>
        <p class="page-desc">
          Explore and manage available tools from internal and external sources.
        </p>
      </div>
      <div class="header-actions">
        <button
          class="btn btn-save"
          @click="reload"
        >
          Reload Tools
        </button>
      </div>
    </div>

    <div
      v-if="sections.length"
      class="tools-grid"
    >
      <section
        v-for="section in sections"
        :key="section.key"
        class="premium-card"
      >
        <div class="card-header-row no-collapse">
          <div class="card-title-group">
            <h2>{{ section.title }}</h2>
          </div>
          <span class="card-summary">{{ section.count }} available</span>
        </div>

        <div class="service-list">
          <div
            v-for="t in section.tools"
            :key="t.name"
            class="service-item tool-row"
            :class="{ disabled: !t.enabled }"
          >
            <div class="service-info">
              <div class="service-name-row">
                <div class="service-name-group">
                  <span class="service-name">{{ t.name }}</span>
                  <Pill
                    :type="t.enabled ? 'ok' : 'muted'"
                    :text="t.enabled ? 'Enabled' : 'Disabled'"
                  />
                </div>
                <div class="service-actions">
                  <button
                    class="btn btn-sm btn-ghost"
                    @click="toggleTool(t)"
                  >
                    {{ t.enabled ? 'Disable' : 'Enable' }}
                  </button>
                </div>
              </div>
              <p class="service-meta description">
                {{ t.description }}
              </p>

              <div
                v-if="t.required?.length"
                class="tool-tags-row"
              >
                <span
                  v-for="r in t.required"
                  :key="r"
                  class="tool-tag"
                >{{ r }}</span>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>

    <div
      v-else
      class="empty-state-container"
    >
      <div class="empty-state">
        No tools loaded. Try reloading your tool directories.
      </div>
    </div>
  </div>
</template>

<style scoped>
.tools-page {
  padding-bottom: 60px;
}

.tools-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
  align-items: start;
}

.tool-row.disabled {
  opacity: 0.6;
}

.service-name-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.service-name-group {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.service-name {
  font-size: 15px;
  font-weight: 700;
}

.service-meta.description {
  margin-top: 8px;
  line-height: 1.55;
}

.tool-tags-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 12px;
}

.empty-state-container {
  padding: 60px 0;
  text-align: center;
}

.empty-state {
  color: var(--muted);
  font-size: 14px;
}

@media (max-width: 960px) {
  .tools-grid {
    grid-template-columns: 1fr;
  }
}
</style>
