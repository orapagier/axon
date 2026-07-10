<script setup>
import { computed, onMounted, ref } from 'vue'
import { get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { useHeaderSearch } from '../lib/headerSearch.js'

const bySource = ref({})
const searchQuery = ref('')

useHeaderSearch('tools', {
  query: searchQuery,
  placeholder: 'Search tools by name or description…',
})

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
    .map(([key, tools]) => ({ key, count: tools.length, tools }))
    .sort((a, b) => a.key.localeCompare(b.key))
)

const allTools = computed(() => Object.values(bySource.value).flat())
const shownCount = computed(() => sections.value.reduce((n, s) => n + s.count, 0))
const enabledCount = computed(() => allTools.value.filter((t) => t.enabled).length)
const isSearching = computed(() => !!searchQuery.value.trim())

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
  // Optimistic: flip immediately, revert if the server rejects it. Waiting on
  // the PUT + a full /tools refetch made the button feel 1-2s slow.
  const next = !t.enabled
  t.enabled = next
  try {
    const r = await put(`/tools/${encodeURIComponent(t.name)}`, { enabled: next })
    if (r?.ok === false) throw new Error(r.error || 'update rejected')
  } catch (e) {
    t.enabled = !next
    toast(`Failed to ${next ? 'enable' : 'disable'} ${t.name}: ${e.message}`, false)
  }
}

onMounted(load)
</script>

<template>
  <div class="page-wrap tools-page">
    <div class="page-toolbar">
      <p class="page-readout">
        <span class="readout-em">{{ shownCount }}</span><template v-if="isSearching">
          / {{ allTools.length }}
        </template> tools
        <template v-if="allTools.length">
          · <span class="readout-em">{{ enabledCount }}</span> enabled
        </template>
      </p>
      <button
        class="btn btn-ghost"
        @click="reload"
      >
        Reload tools
      </button>
    </div>

    <div
      v-if="sections.length"
      class="tools-grid"
    >
      <section
        v-for="section in sections"
        :key="section.key"
        class="panel"
      >
        <div class="panel-head">
          <h2 class="panel-title">
            {{ section.key }}
          </h2>
          <span class="panel-count">{{ section.count }}</span>
        </div>

        <div class="row-list">
          <div
            v-for="t in section.tools"
            :key="t.name"
            class="list-row tool-row"
            :class="{ off: !t.enabled }"
          >
            <div class="row-line">
              <span class="row-title">{{ t.name }}</span>
              <button
                class="switch"
                type="button"
                role="switch"
                :aria-checked="t.enabled ? 'true' : 'false'"
                :aria-label="`${t.enabled ? 'Disable' : 'Enable'} ${t.name}`"
                :title="t.enabled ? 'Disable' : 'Enable'"
                @click="toggleTool(t)"
              />
            </div>
            <p class="row-desc">
              {{ t.description }}
            </p>

            <div
              v-if="t.required?.length"
              class="chip-row"
            >
              <span
                v-for="r in t.required"
                :key="r"
                class="mono-chip"
              >{{ r }}</span>
            </div>
          </div>
        </div>
      </section>
    </div>

    <div
      v-else
      class="empty-state"
    >
      <p class="empty-title">
        {{ isSearching ? 'No matching tools' : 'No tools loaded' }}
      </p>
      <p class="empty-hint">
        {{ isSearching ? `Nothing matches "${searchQuery.trim()}". Try a different term.` : 'Reload tools to scan the tool directories.' }}
      </p>
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
  gap: 14px;
  align-items: start;
}

/* A tool that isn't firing fades back into the membrane. */
.tool-row.off .row-title {
  color: var(--muted);
}

.tool-row.off .row-desc,
.tool-row.off .chip-row {
  opacity: 0.55;
}

@media (max-width: 960px) {
  .tools-grid {
    grid-template-columns: 1fr;
  }
}
</style>
