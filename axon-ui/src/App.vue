<script setup>
import { ref, computed, defineAsyncComponent, markRaw, onMounted, onUnmounted } from 'vue'
import { wsStatus } from './lib/ws.js'
import { confirmDialog } from './lib/confirm.js'
import { headerSearchFor } from './lib/headerSearch.js'
import ConfirmDialog from './components/ConfirmDialog.vue'
import PromptDialog from './components/PromptDialog.vue'
import NotificationBell from './components/NotificationBell.vue'
import ToastHost from './components/ToastHost.vue'
import SearchInput from './components/SearchInput.vue'
import LoginPage from './pages/LoginPage.vue'

const PAGES = {
  chat: markRaw(defineAsyncComponent(() => import('./pages/ChatPage.vue'))),
  models: markRaw(defineAsyncComponent(() => import('./pages/ModelsPage.vue'))),
  tools: markRaw(defineAsyncComponent(() => import('./pages/ToolsPage.vue'))),
  memories: markRaw(defineAsyncComponent(() => import('./pages/MemoriesPage.vue'))),
  tasks: markRaw(defineAsyncComponent(() => import('./pages/TasksPage.vue'))),
  workflows: markRaw(defineAsyncComponent(() => import('./pages/WorkflowsPage.vue'))),
  crm: markRaw(defineAsyncComponent(() => import('./pages/CrmPage.vue'))),
  services: markRaw(defineAsyncComponent(() => import('./pages/ServicesPage.vue'))),
  files: markRaw(defineAsyncComponent(() => import('./pages/FilesPage.vue'))),
  docs: markRaw(defineAsyncComponent(() => import('./pages/DocsPage.vue'))),
  settings: markRaw(defineAsyncComponent(() => import('./pages/SettingsPage.vue'))),
}

const NAV = [
  {
    id: 'chat',
    label: 'Chat',
    description: 'Agent conversations and runs',
    tint: '#3ecfae',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 4h14a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H9l-5 4V6a2 2 0 0 1 2-2Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M8.5 9h7M8.5 12.5h4.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  },
  {
    id: 'models',
    label: 'Models',
    description: 'Providers, routing, and quotas',
    tint: '#6ea3ef',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="2"/><path d="M10 2.5V6M14 2.5V6M10 18v3.5M14 18v3.5M2.5 10H6M2.5 14H6M18 10h3.5M18 14h3.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  },
  {
    id: 'tools',
    label: 'Tools',
    description: 'Runtime tool inventory',
    tint: '#948ceb',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'memories',
    label: 'Memories',
    description: 'Short and long term context',
    tint: '#d879c9',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.5 2A2.5 2.5 0 0 1 12 4.5v15a2.5 2.5 0 0 1-4.96.44 2.5 2.5 0 0 1-2.96-3.08 3 3 0 0 1-.34-5.58 2.5 2.5 0 0 1 1.32-4.24 2.5 2.5 0 0 1 1.98-3A2.5 2.5 0 0 1 9.5 2Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M14.5 2A2.5 2.5 0 0 0 12 4.5v15a2.5 2.5 0 0 0 4.96.44 2.5 2.5 0 0 0 2.96-3.08 3 3 0 0 0 .34-5.58 2.5 2.5 0 0 0-1.32-4.24 2.5 2.5 0 0 0-1.98-3A2.5 2.5 0 0 0 14.5 2Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'tasks',
    label: 'Tasks',
    description: 'Schedulers and automation jobs',
    tint: '#d9ac55',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 2v4M16 2v4M4 10h16M6 5h12a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="m10 14 1.5 1.5L15 12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'workflows',
    label: 'Workflows',
    description: 'Visual automation canvas',
    tint: '#e8808f',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="6" r="2.5" fill="none" stroke="currentColor" stroke-width="2"/><circle cx="18" cy="12" r="2.5" fill="none" stroke="currentColor" stroke-width="2"/><circle cx="6" cy="18" r="2.5" fill="none" stroke="currentColor" stroke-width="2"/><path d="M8.5 7.2 15.5 10.8M8.5 16.8l7-3.6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  },
  {
    id: 'crm',
    label: 'CRM',
    description: 'Leads, deals, and organizations',
    tint: '#e07bb5',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M16 21v-2a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v2" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><circle cx="9.5" cy="7.5" r="3.5" fill="none" stroke="currentColor" stroke-width="2"/><path d="M21 21v-2a4 4 0 0 0-3-3.87M15.5 4.13a3.5 3.5 0 0 1 0 6.75" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  },
  {
    id: 'services',
    label: 'Services',
    description: 'External integrations and auth',
    tint: '#4cc98a',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 2.5V8M15 2.5V8M6 8h12l-1.2 5.5a5 5 0 0 1-9.6 0L6 8Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M12 17.5v4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  },
  {
    id: 'files',
    label: 'Files',
    description: 'Shared uploads and outputs',
    tint: '#8b94ea',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h4.2a2 2 0 0 1 1.4.6L12.4 7H19a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'docs',
    label: 'Docs',
    description: 'Searchable product documentation',
    tint: '#cfa953',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 6c-1.8-1.3-4.3-2-8-2v14c3.7 0 6.2.7 8 2 1.8-1.3 4.3-2 8-2V4c-3.7 0-6.2.7-8 2Z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M12 6v14" fill="none" stroke="currentColor" stroke-width="2"/></svg>',
  },
  {
    id: 'settings',
    label: 'Settings',
    description: 'System configuration',
    tint: '#97a2b2',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 8.5A3.5 3.5 0 1 0 12 15.5 3.5 3.5 0 0 0 12 8.5Z" fill="none" stroke="currentColor" stroke-width="2"/><path d="M19.4 15a1 1 0 0 0 .2 1.1l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1 1 0 0 0-1.1-.2 1 1 0 0 0-.6.9V20a2 2 0 1 1-4 0v-.2a1 1 0 0 0-.6-.9 1 1 0 0 0-1.1.2l-.1.1a2 2 0 0 1-2.8-2.8l.1-.1a1 1 0 0 0 .2-1.1 1 1 0 0 0-.9-.6H4a2 2 0 1 1 0-4h.2a1 1 0 0 0 .9-.6 1 1 0 0 0-.2-1.1l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1 1 0 0 0 1.1.2 1 1 0 0 0 .6-.9V4a2 2 0 1 1 4 0v.2a1 1 0 0 0 .6.9 1 1 0 0 0 1.1-.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1 1 0 0 0-.2 1.1 1 1 0 0 0 .9.6H20a2 2 0 1 1 0 4h-.2a1 1 0 0 0-.9.6Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/></svg>',
  },
]

const activePage = ref('chat')
// Chat stays mounted once visited so navigating away doesn't wipe the
// conversation. Other pages unmount so their polling timers stop.
const KEEP_MOUNTED = new Set(['chat'])
const visitedPages = ref(new Set(['chat']))
const sidebarOpen = ref(false)
// Chat is the default landing page and, like Workflows, wants the full canvas —
// so start collapsed. navigate() keeps it in sync on every page change.
const isSidebarCollapsed = ref(true)
const isAuthenticated = ref(!!localStorage.getItem('AXON_MASTER_KEY'))

const activeNav = computed(() => NAV.find((item) => item.id === activePage.value) || NAV[0])

function navigate(id) {
  activePage.value = id
  visitedPages.value.add(id)
  isSidebarCollapsed.value = id === 'workflows' || id === 'chat'

  if (window.innerWidth < 1024) {
    sidebarOpen.value = false
    isSidebarCollapsed.value = true
    return
  }

  // On desktop/tablet we auto-close (collapse) for Workflows and Chat (both want
  // the full canvas) and auto-expand elsewhere.
  sidebarOpen.value = false
}

function toggleSidebar() {
  isSidebarCollapsed.value = !isSidebarCollapsed.value
}

const wsDotClass = computed(() => {
  if (wsStatus.value === 'connected') return 'connected'
  if (wsStatus.value === 'connecting') return 'connecting'
  return 'disconnected'
})

const wsLabel = computed(() => {
  if (wsStatus.value === 'connected') return 'Connected'
  if (wsStatus.value === 'connecting') return 'Connecting'
  return 'Reconnecting'
})

// The topbar search field: bound to whatever scope the active page registered
// via useHeaderSearch(); hidden on pages with nothing to search.
const topbarSearch = headerSearchFor(activePage)
const topbarSearchRef = ref(null)

// Pages no longer autofocus a local search on mount, so "/" focuses the
// topbar field from anywhere (unless the user is already typing somewhere).
function onGlobalKeydown(e) {
  if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return
  const t = e.target
  if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return
  if (!topbarSearch.value) return
  e.preventDefault()
  topbarSearchRef.value?.focus()
}
onMounted(() => window.addEventListener('keydown', onGlobalKeydown))
onUnmounted(() => window.removeEventListener('keydown', onGlobalKeydown))

function reload() {
  window.location.reload()
}

async function logout() {
  const ok = await confirmDialog('You will need to sign in again to access the dashboard.', {
    title: 'Logout',
    confirmText: 'Logout',
    danger: false,
  })
  if (!ok) return
  localStorage.removeItem('AXON_MASTER_KEY')
  isAuthenticated.value = false
  window.location.reload()
}
</script>

<template>
  <div v-if="!isAuthenticated">
    <LoginPage @login="isAuthenticated = true" />
  </div>
  <div
    v-else
    class="layout app-shell"
    :class="{ 'sidebar-collapsed': isSidebarCollapsed, 'sidebar-open': sidebarOpen }"
  >
    <aside
      class="sidebar"
      :class="{ open: sidebarOpen, collapsed: isSidebarCollapsed }"
    >
      <div class="sidebar-panel">
        <div class="sidebar-header-row">
          <div class="sidebar-brand-stack">
            <button
              class="brand-lockup"
              type="button"
              :title="isSidebarCollapsed ? 'Reload dashboard' : ''"
              @click="reload"
            >
              <img
                src="/favicon.png"
                alt="Axon"
                class="logo-img"
              >
              <div
                v-if="!isSidebarCollapsed"
                class="brand-copy"
              >
                <span class="logo-text">AXON</span>
                <span class="logo-subtitle">Agent Dashboard</span>
              </div>
            </button>

            <button
              class="shell-icon-btn shell-nav-toggle desktop-only"
              type="button"
              :title="isSidebarCollapsed ? 'Expand navigation' : 'Collapse navigation'"
              @click="toggleSidebar"
            >
              <svg
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <path
                  d="M6 5v14"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                />
                <path
                  :d="isSidebarCollapsed ? 'M11 8l4 4-4 4' : 'M15 8l-4 4 4 4'"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </button>
          </div>

          <button
            class="shell-icon-btn shell-nav-toggle mobile-only"
            type="button"
            title="Close navigation"
            @click="sidebarOpen = false"
          >
            <svg
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path
                d="M18 6 6 18M6 6l12 12"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
            </svg>
          </button>
        </div>

        <nav class="sidebar-nav">
          <button
            v-for="item in NAV"
            :key="item.id"
            class="nav-item"
            :class="{ active: activePage === item.id }"
            :style="{ '--nav-tint': item.tint }"
            :title="isSidebarCollapsed ? item.label : ''"
            type="button"
            @click="navigate(item.id)"
          >
            <span
              class="nav-icon"
              v-html="item.icon"
            />
            <span
              v-if="!isSidebarCollapsed"
              class="nav-label"
            >{{ item.label }}</span>
          </button>
        </nav>
      </div>
    </aside>

    <div
      v-if="sidebarOpen"
      class="sidebar-overlay open"
      @click="sidebarOpen = false"
    />

    <main class="main">
      <header class="shell-topbar">
        <div class="shell-topbar-left">
          <button
            class="shell-icon-btn shell-nav-toggle mobile-only"
            type="button"
            title="Open navigation"
            @click="sidebarOpen = !sidebarOpen"
          >
            <svg
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path
                d="M4 7h16M4 12h16M4 17h16"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
              />
            </svg>
          </button>

          <div
            class="shell-page-meta"
            :style="{ '--page-tint': activeNav.tint }"
          >
            <div class="shell-page-heading-row">
              <span
                class="shell-page-dot"
                aria-hidden="true"
              />
              <div class="shell-page-heading">
                {{ activeNav.label }}
              </div>
              <span class="shell-page-pill">{{ activeNav.description }}</span>
            </div>
          </div>
        </div>

        <div class="shell-topbar-right">
          <SearchInput
            v-if="topbarSearch"
            ref="topbarSearchRef"
            v-model="topbarSearch.query"
            class="shell-topbar-search"
            :autofocus="false"
            :placeholder="topbarSearch.placeholder"
            @keyup.enter="topbarSearch.onSubmit?.()"
          />
          <NotificationBell />
          <div class="shell-status-chip">
            <span
              class="ws-dot"
              :class="wsDotClass"
            />
            <span>{{ wsLabel }}</span>
          </div>
          <button
            class="btn btn-danger shell-logout-btn"
            type="button"
            @click="logout"
          >
            Logout
          </button>
        </div>
      </header>

      <div class="main-scroll">
        <section
          v-for="item in NAV"
          :id="`page-${item.id}`"
          :key="item.id"
          class="page"
          :class="{ active: activePage === item.id, 'page-flush': item.id === 'workflows' }"
          :style="{ display: activePage === item.id ? 'flex' : 'none' }"
        >
          <component
            :is="PAGES[item.id]"
            v-if="activePage === item.id || (KEEP_MOUNTED.has(item.id) && visitedPages.has(item.id))"
          />
        </section>
      </div>
    </main>
  </div>

  <ConfirmDialog />
  <PromptDialog />
  <ToastHost />
</template>
