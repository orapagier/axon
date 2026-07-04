<script setup>
import { ref, computed, defineAsyncComponent, markRaw } from 'vue'
import { wsStatus } from './lib/ws.js'
import Toast from './components/Toast.vue'
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
    tint: '#5eead4',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 10h10M7 14h6M6 19l-2 2V5a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H6Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'models',
    label: 'Models',
    description: 'Providers, routing, and quotas',
    tint: '#8ec5ff',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3Zm0 0v18M4 7.5l8 4.5 8-4.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'tools',
    label: 'Tools',
    description: 'Runtime tool inventory',
    tint: '#c4b5fd',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14 5a4 4 0 0 0 5 5l-8.5 8.5a2.12 2.12 0 1 1-3-3L16 7a4 4 0 0 0-2-2Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="M5 19 3 21" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'memories',
    label: 'Memories',
    description: 'Short and long term context',
    tint: '#f0abfc',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.5 4A3.5 3.5 0 0 0 6 7.5v1A2.5 2.5 0 0 0 3.5 11v2A2.5 2.5 0 0 0 6 15.5v1A3.5 3.5 0 0 0 9.5 20h5a3.5 3.5 0 0 0 3.5-3.5v-1A2.5 2.5 0 0 0 20.5 13v-2A2.5 2.5 0 0 0 18 8.5v-1A3.5 3.5 0 0 0 14.5 4h-5Z" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M9 9h6M9 12h6M9 15h3" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'tasks',
    label: 'Tasks',
    description: 'Schedulers and automation jobs',
    tint: '#fbd38d',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 2v4M16 2v4M4 10h16M6 5h12a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><path d="m10 14 1.5 1.5L15 12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'workflows',
    label: 'Workflows',
    description: 'Visual automation canvas',
    tint: '#fda4af',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="6" r="2.5" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="18" cy="12" r="2.5" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="6" cy="18" r="2.5" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M8.5 7.2 15.5 10.8M8.5 16.8l7-3.6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'crm',
    label: 'CRM',
    description: 'Leads, deals, and organizations',
    tint: '#f9a8d4',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M16 21v-2a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><circle cx="9.5" cy="7.5" r="3.5" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M21 21v-2a4 4 0 0 0-3-3.87M15.5 4.13a3.5 3.5 0 0 1 0 6.75" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'services',
    label: 'Services',
    description: 'External integrations and auth',
    tint: '#86efac',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2 4 6v6c0 5 3.4 9.7 8 10 4.6-.3 8-5 8-10V6l-8-4Z" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M9.5 12 11 13.5 14.5 10" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  },
  {
    id: 'files',
    label: 'Files',
    description: 'Shared uploads and outputs',
    tint: '#a5b4fc',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M14 3v5h5M9 13h6M9 17h6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'docs',
    label: 'Docs',
    description: 'Searchable product documentation',
    tint: '#fde68a',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5a2 2 0 0 1 2-2h5v16H6a2 2 0 0 0-2 2V5Zm16 0a2 2 0 0 0-2-2h-5v16h5a2 2 0 0 1 2 2V5Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M8 7h1.5M8 10h1.5M15 7h1.5M15 10h1.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
  },
  {
    id: 'settings',
    label: 'Settings',
    description: 'System configuration',
    tint: '#cbd5e1',
    icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 8.5A3.5 3.5 0 1 0 12 15.5 3.5 3.5 0 0 0 12 8.5Z" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M19.4 15a1 1 0 0 0 .2 1.1l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1 1 0 0 0-1.1-.2 1 1 0 0 0-.6.9V20a2 2 0 1 1-4 0v-.2a1 1 0 0 0-.6-.9 1 1 0 0 0-1.1.2l-.1.1a2 2 0 0 1-2.8-2.8l.1-.1a1 1 0 0 0 .2-1.1 1 1 0 0 0-.9-.6H4a2 2 0 1 1 0-4h.2a1 1 0 0 0 .9-.6 1 1 0 0 0-.2-1.1l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1 1 0 0 0 1.1.2 1 1 0 0 0 .6-.9V4a2 2 0 1 1 4 0v.2a1 1 0 0 0 .6.9 1 1 0 0 0 1.1-.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1 1 0 0 0-.2 1.1 1 1 0 0 0 .9.6H20a2 2 0 1 1 0 4h-.2a1 1 0 0 0-.9.6Z" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>',
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

function reload() {
  window.location.reload()
}

function logout() {
  if (!window.confirm('Logout from dashboard?')) return
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
    <div class="app-shell-backdrop" aria-hidden="true">
      <span class="shell-ambient shell-ambient-one"></span>
      <span class="shell-ambient shell-ambient-two"></span>
      <span class="shell-ambient shell-ambient-three"></span>
    </div>

    <aside class="sidebar" :class="{ open: sidebarOpen, collapsed: isSidebarCollapsed }">
      <div class="sidebar-panel">
        <div class="sidebar-header-row">
          <div class="sidebar-brand-stack">
            <button class="brand-lockup" type="button" @click="reload" :title="isSidebarCollapsed ? 'Reload dashboard' : ''">
              <img src="/favicon.png" alt="Axon" class="logo-img" />
              <div v-if="!isSidebarCollapsed" class="brand-copy">
                <span class="logo-text">AXON</span>
                <span class="logo-subtitle">Agent Dashboard</span>
              </div>
            </button>

            <button
              class="shell-icon-btn shell-nav-toggle desktop-only"
              type="button"
              @click="toggleSidebar"
              :title="isSidebarCollapsed ? 'Expand navigation' : 'Collapse navigation'"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M6 5v14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
                <path :d="isSidebarCollapsed ? 'M11 8l4 4-4 4' : 'M15 8l-4 4 4 4'" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
            </button>
          </div>

          <button class="shell-icon-btn shell-nav-toggle mobile-only" type="button" @click="sidebarOpen = false" title="Close navigation">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M18 6 6 18M6 6l12 12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
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
            <span class="nav-icon" v-html="item.icon"></span>
            <span v-if="!isSidebarCollapsed" class="nav-copy">
              <span class="nav-label">{{ item.label }}</span>
              <span class="nav-description">{{ item.description }}</span>
            </span>
          </button>
        </nav>

      </div>
    </aside>

    <div v-if="sidebarOpen" class="sidebar-overlay open" @click="sidebarOpen = false"></div>

    <main class="main">
      <header class="shell-topbar">
        <div class="shell-topbar-left">
          <button class="shell-icon-btn shell-nav-toggle mobile-only" type="button" @click="sidebarOpen = !sidebarOpen" title="Open navigation">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 7h16M4 12h16M4 17h16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
            </svg>
          </button>

          <div class="shell-page-meta" :style="{ '--page-tint': activeNav.tint }">
            <div class="shell-page-heading-row">
              <span class="shell-page-dot" aria-hidden="true"></span>
              <div class="shell-page-heading">{{ activeNav.label }}</div>
              <span class="shell-page-pill">{{ activeNav.description }}</span>
            </div>
          </div>
        </div>

        <div class="shell-topbar-right">
          <button class="shell-icon-btn shell-refresh-btn desktop-only" type="button" @click="reload" title="Refresh workspace">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M20 11a8 8 0 1 0 2 5.3" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
              <path d="M20 4v7h-7" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          </button>
          <div class="shell-status-chip">
            <span class="ws-dot" :class="wsDotClass"></span>
            <span>{{ wsLabel }}</span>
          </div>
          <button class="btn btn-danger shell-logout-btn" type="button" @click="logout">
            Logout
          </button>
        </div>
      </header>

      <div class="main-scroll">
        <section
          v-for="item in NAV"
          :key="item.id"
          class="page"
          :class="{ active: activePage === item.id, 'page-flush': item.id === 'workflows' }"
          :style="{ display: activePage === item.id ? 'flex' : 'none' }"
          :id="`page-${item.id}`"
        >
          <component
            v-if="activePage === item.id || (KEEP_MOUNTED.has(item.id) && visitedPages.has(item.id))"
            :is="PAGES[item.id]"
          />
        </section>
      </div>
    </main>
  </div>

  <Toast />
</template>
