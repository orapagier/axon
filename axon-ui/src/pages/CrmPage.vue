<script setup>
import { computed, onMounted, ref } from 'vue'
import { get, post, put } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { confirmDialog } from '../lib/confirm.js'
import { timeAgo } from '../lib/utils.js'
import Modal from '../components/Modal.vue'
import Pill from '../components/Pill.vue'

const LEAD_STATUSES = ['Open', 'Contacted', 'Qualified', 'Lost']
const DEAL_STAGES = ['Prospecting', 'Qualified', 'Proposal', 'Negotiation', 'Won', 'Lost']
const ACTIVITY_KINDS = ['note', 'call', 'email', 'meeting', 'task', 'other']
const PLURAL = { lead: 'leads', deal: 'deals', org: 'orgs', activity: 'activities' }

const TABS = [
  { id: 'dashboard', label: 'Dashboard' },
  { id: 'leads', label: 'Leads' },
  { id: 'deals', label: 'Deals' },
  { id: 'orgs', label: 'Organizations' },
  { id: 'archived', label: 'Archived' },
]
const tab = ref('dashboard')

// ── Tool responses come back verbatim; errors are {error, message}. ─────────
function failed(r) {
  return !r || r.error
}
function errMsg(r) {
  if (!r) return 'Request failed'
  if (typeof r.message === 'string') return r.message
  if (typeof r.error === 'string') return r.error
  return 'Request failed'
}

function fmtMoney(amount, currency) {
  const n = Number(amount || 0)
  return `${n.toLocaleString(undefined, { maximumFractionDigits: 2 })} ${currency || ''}`.trim()
}
// Per-currency totals object {USD: 125000, EUR: 4000} → "125,000 USD · 4,000 EUR"
function fmtTotals(tv) {
  const entries = Object.entries(tv || {})
  if (!entries.length) return '—'
  return entries.map(([c, v]) => fmtMoney(v, c)).join(' · ')
}
function fmtDate(iso) {
  return iso ? String(iso).slice(0, 10) : '—'
}
function leadStatusType(s) {
  if (s === 'Qualified') return 'ok'
  if (s === 'Contacted') return 'warn'
  if (s === 'Lost') return 'muted'
  return 'info'
}
function dealStageType(s) {
  if (s === 'Won') return 'ok'
  if (s === 'Lost') return 'err'
  return 'info'
}

// ── Dashboard ────────────────────────────────────────────────────────────────
const dash = ref(null)
const pipeline = ref(null)

async function loadDashboard() {
  const [d, p] = await Promise.all([get('/crm/dashboard'), get('/crm/pipeline')])
  if (!failed(d)) dash.value = d
  else toast(errMsg(d), false)
  if (!failed(p)) pipeline.value = p
}

// ── Leads ────────────────────────────────────────────────────────────────────
const leads = ref([])
const leadTotal = ref(0)
const leadStatus = ref('All')
const leadQ = ref('')

async function loadLeads() {
  const params = new URLSearchParams({ limit: '200' })
  const q = leadQ.value.trim()
  if (q) params.set('q', q)
  else if (leadStatus.value !== 'All') params.set('status', leadStatus.value)
  const r = await get(`/crm/leads?${params}`)
  if (failed(r)) return toast(errMsg(r), false)
  leads.value = r.leads || r.results || []
  leadTotal.value = r.total ?? leads.value.length
}

async function setLeadStatus(lead, status) {
  const r = await put(`/crm/leads/${lead.id}`, { status })
  if (failed(r)) return toast(errMsg(r), false)
  lead.status = status
  toast(`Lead marked ${status}`, true)
}

// ── Deals (kanban) ───────────────────────────────────────────────────────────
const deals = ref([])
const dealTotal = ref(0)
const dealQ = ref('')

async function loadDeals() {
  const params = new URLSearchParams({ limit: '200' })
  const q = dealQ.value.trim()
  if (q) params.set('q', q)
  const [r, p] = await Promise.all([get(`/crm/deals?${params}`), get('/crm/pipeline')])
  if (failed(r)) return toast(errMsg(r), false)
  deals.value = r.deals || r.results || []
  dealTotal.value = r.total ?? deals.value.length
  if (!failed(p)) pipeline.value = p
}

const dealsByStage = computed(() => {
  const by = Object.fromEntries(DEAL_STAGES.map((s) => [s, []]))
  for (const d of deals.value) if (by[d.stage]) by[d.stage].push(d)
  return by
})

// Column header counts/totals come from the pipeline summary so they cover the
// whole DB even when the card list is truncated at 200.
function stageMeta(stage) {
  const entry = (pipeline.value?.pipeline || []).find((s) => s.stage === stage)
  return entry || { count: dealsByStage.value[stage]?.length || 0, total_value: {} }
}

async function setDealStage(deal, stage) {
  const r = await put(`/crm/deals/${deal.id}`, { stage })
  if (failed(r)) return toast(errMsg(r), false)
  toast(`Deal moved to ${stage}`, true)
  loadDeals()
}

// ── Organizations ────────────────────────────────────────────────────────────
const orgs = ref([])
const orgTotal = ref(0)
const orgQ = ref('')

async function loadOrgs() {
  const params = new URLSearchParams({ limit: '200' })
  const q = orgQ.value.trim()
  if (q) params.set('q', q)
  const r = await get(`/crm/orgs?${params}`)
  if (failed(r)) return toast(errMsg(r), false)
  orgs.value = r.organizations || r.results || []
  orgTotal.value = r.total ?? orgs.value.length
}

// ── Archived ─────────────────────────────────────────────────────────────────
const archived = ref([])

async function loadArchived() {
  const r = await get('/crm/archived?limit=200')
  if (failed(r)) return toast(errMsg(r), false)
  archived.value = r.archived_records || []
}

async function archiveRecord(type, id) {
  const ok = await confirmDialog('It can be restored later from the Archived tab.', {
    title: `Archive this ${type}`,
    confirmText: 'Archive',
    danger: false,
  })
  if (!ok) return
  const r = await post(`/crm/${PLURAL[type]}/${id}/archive`, {})
  if (failed(r)) return toast(errMsg(r), false)
  toast('Archived', true)
  closeDrawer()
  reloadTab()
}

async function restoreRecord(type, id) {
  const r = await post(`/crm/${PLURAL[type]}/${id}/restore`, {})
  if (failed(r)) return toast(errMsg(r), false)
  toast('Restored', true)
  loadArchived()
}

// ── Record drawer (360° overview) ────────────────────────────────────────────
const drawer = ref({ open: false, type: null, id: null, data: null, loading: false })
const actForm = ref({ kind: 'note', title: '', body: '' })

async function openDrawer(type, id) {
  drawer.value = { open: true, type, id, data: null, loading: true }
  actForm.value = { kind: 'note', title: '', body: '' }
  const r = await get(`/crm/overview/${PLURAL[type]}/${id}`)
  drawer.value.loading = false
  if (failed(r)) {
    toast(errMsg(r), false)
    drawer.value.open = false
    return
  }
  drawer.value.data = r
}

function closeDrawer() {
  drawer.value = { open: false, type: null, id: null, data: null, loading: false }
}

async function refreshDrawer() {
  if (drawer.value.open) await openDrawer(drawer.value.type, drawer.value.id)
}

async function logActivity() {
  if (!actForm.value.title.trim()) return toast('Activity title is required', false)
  const body = {
    entity_id: drawer.value.id,
    entity_type: drawer.value.type,
    kind: actForm.value.kind,
    title: actForm.value.title.trim(),
  }
  if (actForm.value.body.trim()) body.body = actForm.value.body.trim()
  const r = await post('/crm/activities', body)
  if (failed(r)) return toast(errMsg(r), false)
  toast('Activity logged', true)
  actForm.value = { kind: 'note', title: '', body: '' }
  refreshDrawer()
}

function reloadTab() {
  if (tab.value === 'dashboard') loadDashboard()
  else if (tab.value === 'leads') loadLeads()
  else if (tab.value === 'deals') loadDeals()
  else if (tab.value === 'orgs') loadOrgs()
  else if (tab.value === 'archived') loadArchived()
}

function switchTab(id) {
  tab.value = id
  reloadTab()
}

// ── Create / edit modals ─────────────────────────────────────────────────────
// Update bodies only carry non-empty fields, so an untouched blank input never
// overwrites stored data (clearing a field is done by editing it via chat).
function cleanBody(form, stringFields) {
  const body = {}
  for (const f of stringFields) {
    const v = String(form[f] ?? '').trim()
    if (v) body[f] = v
  }
  if (typeof form.tags === 'string' && form.tags.trim()) {
    body.tags = form.tags.split(',').map((t) => t.trim()).filter(Boolean)
  }
  return body
}
// <input type="date"> gives YYYY-MM-DD; the CRM stores full RFC3339 UTC.
function dateToRfc3339(d) {
  return /^\d{4}-\d{2}-\d{2}$/.test(d) ? `${d}T00:00:00Z` : d
}

const leadModalOpen = ref(false)
const editingLeadId = ref(null)
const leadForm = ref({})

function openLeadCreate() {
  editingLeadId.value = null
  leadForm.value = { name: '', email: '', phone: '', company: '', status: 'Open', source: '', tags: '', notes: '' }
  leadModalOpen.value = true
}

async function openLeadEdit(id) {
  const r = await get(`/crm/leads/${id}`)
  if (failed(r)) return toast(errMsg(r), false)
  editingLeadId.value = id
  leadForm.value = {
    name: r.name || '',
    email: r.email || '',
    phone: r.phone || '',
    company: r.company || '',
    status: r.status || 'Open',
    source: r.source || '',
    tags: (r.tags || []).join(', '),
    notes: r.notes || '',
  }
  leadModalOpen.value = true
}

async function saveLead() {
  if (!leadForm.value.name.trim()) return toast('Name is required', false)
  const body = cleanBody(leadForm.value, ['name', 'email', 'phone', 'company', 'status', 'source', 'notes'])
  const r = editingLeadId.value
    ? await put(`/crm/leads/${editingLeadId.value}`, body)
    : await post('/crm/leads', body)
  if (failed(r)) return toast(errMsg(r), false)
  toast(editingLeadId.value ? 'Lead updated' : 'Lead created', true)
  leadModalOpen.value = false
  loadLeads()
  refreshDrawer()
}

const dealModalOpen = ref(false)
const editingDealId = ref(null)
const dealForm = ref({})

async function openDealCreate(contactId) {
  editingDealId.value = null
  dealForm.value = {
    title: '',
    amount: '',
    currency: 'USD',
    stage: 'Prospecting',
    probability: '',
    contact_id: contactId || '',
    expected_close: '',
    tags: '',
    notes: '',
  }
  if (!leads.value.length) await loadLeads()
  dealModalOpen.value = true
}

async function openDealEdit(id) {
  const r = await get(`/crm/deals/${id}`)
  if (failed(r)) return toast(errMsg(r), false)
  if (!leads.value.length) await loadLeads()
  editingDealId.value = id
  dealForm.value = {
    title: r.title || '',
    amount: r.amount ?? '',
    currency: r.currency || 'USD',
    stage: r.stage || 'Prospecting',
    probability: r.probability ?? '',
    contact_id: r.contact_id || '',
    expected_close: r.expected_close ? String(r.expected_close).slice(0, 10) : '',
    tags: (r.tags || []).join(', '),
    notes: r.notes || '',
  }
  dealModalOpen.value = true
}

async function saveDeal() {
  if (!dealForm.value.title.trim()) return toast('Title is required', false)
  if (!editingDealId.value && !dealForm.value.contact_id) return toast('A contact (lead) is required', false)
  const body = cleanBody(dealForm.value, ['title', 'currency', 'stage', 'contact_id', 'notes'])
  if (dealForm.value.amount !== '' && dealForm.value.amount !== null) {
    const amount = Number(dealForm.value.amount)
    if (Number.isNaN(amount) || amount < 0) return toast('Amount must be a non-negative number', false)
    body.amount = amount
  }
  if (dealForm.value.probability !== '' && dealForm.value.probability !== null) {
    const probability = Number(dealForm.value.probability)
    if (!Number.isInteger(probability) || probability < 0 || probability > 100)
      return toast('Probability must be 0–100', false)
    body.probability = probability
  }
  if (dealForm.value.expected_close) body.expected_close = dateToRfc3339(dealForm.value.expected_close)
  const r = editingDealId.value
    ? await put(`/crm/deals/${editingDealId.value}`, body)
    : await post('/crm/deals', body)
  if (failed(r)) return toast(errMsg(r), false)
  toast(editingDealId.value ? 'Deal updated' : 'Deal created', true)
  dealModalOpen.value = false
  loadDeals()
  refreshDrawer()
}

const orgModalOpen = ref(false)
const editingOrgId = ref(null)
const orgForm = ref({})

function openOrgCreate() {
  editingOrgId.value = null
  orgForm.value = { name: '', website: '', industry: '', size: '', country: '', phone: '', email: '', tags: '', notes: '' }
  orgModalOpen.value = true
}

async function openOrgEdit(id) {
  const r = await get(`/crm/orgs/${id}`)
  if (failed(r)) return toast(errMsg(r), false)
  editingOrgId.value = id
  orgForm.value = {
    name: r.name || '',
    website: r.website || '',
    industry: r.industry || '',
    size: r.size || '',
    country: r.country || '',
    phone: r.phone || '',
    email: r.email || '',
    tags: (r.tags || []).join(', '),
    notes: r.notes || '',
  }
  orgModalOpen.value = true
}

async function saveOrg() {
  if (!orgForm.value.name.trim()) return toast('Name is required', false)
  const body = cleanBody(orgForm.value, ['name', 'website', 'industry', 'size', 'country', 'phone', 'email', 'notes'])
  const r = editingOrgId.value
    ? await put(`/crm/orgs/${editingOrgId.value}`, body)
    : await post('/crm/orgs', body)
  if (failed(r)) return toast(errMsg(r), false)
  toast(editingOrgId.value ? 'Organization updated' : 'Organization created', true)
  orgModalOpen.value = false
  loadOrgs()
  refreshDrawer()
}

function editDrawerRecord() {
  const { type, id } = drawer.value
  if (type === 'lead') openLeadEdit(id)
  else if (type === 'deal') openDealEdit(id)
  else if (type === 'org') openOrgEdit(id)
}

onMounted(loadDashboard)
</script>

<template>
  <div class="services-page crm-page">
    <div class="page-header-container">
      <div class="page-header">
        <h1>CRM</h1>
        <p class="page-desc">Leads, deals, organizations, and activity — the agent's customer data.</p>
      </div>
      <div class="header-actions">
        <button v-if="tab === 'leads'" class="btn btn-save" @click="openLeadCreate">New Lead</button>
        <button v-if="tab === 'deals'" class="btn btn-save" @click="openDealCreate()">New Deal</button>
        <button v-if="tab === 'orgs'" class="btn btn-save" @click="openOrgCreate">New Organization</button>
        <button class="btn btn-ghost" @click="reloadTab">Refresh</button>
      </div>
    </div>

    <div class="crm-tabs">
      <button
        v-for="t in TABS"
        :key="t.id"
        class="crm-tab"
        :class="{ active: tab === t.id }"
        @click="switchTab(t.id)"
      >
        {{ t.label }}
      </button>
    </div>

    <!-- ── Dashboard ─────────────────────────────────────────────────────── -->
    <template v-if="tab === 'dashboard'">
      <div v-if="!dash" class="empty-state">Loading dashboard…</div>
      <template v-else>
        <div class="stat-tiles">
          <div class="premium-card stat-tile">
            <div class="stat-value">{{ dash.totals.organizations }}</div>
            <div class="stat-label">Organizations</div>
          </div>
          <div class="premium-card stat-tile">
            <div class="stat-value">{{ dash.totals.leads }}</div>
            <div class="stat-label">Leads</div>
          </div>
          <div class="premium-card stat-tile">
            <div class="stat-value">{{ dash.totals.deals }}</div>
            <div class="stat-label">Deals</div>
          </div>
          <div class="premium-card stat-tile">
            <div class="stat-value">{{ dash.totals.recent_activities }}</div>
            <div class="stat-label">Activities ({{ dash.parameters.activity_window_days }}d)</div>
          </div>
        </div>

        <div class="dash-grid">
          <section class="premium-card">
            <div class="card-header-row no-collapse">
              <div class="card-title-group"><h2>Pipeline Health</h2></div>
              <span v-if="pipeline" class="card-summary">win rate {{ pipeline.win_rate_pct }}%</span>
            </div>
            <div class="card-content kv-list">
              <div class="kv-row"><span>Active pipeline</span><strong>{{ fmtTotals(dash.pipeline.active_pipeline_value) }}</strong></div>
              <div class="kv-row"><span>Weighted (by probability)</span><strong>{{ fmtTotals(dash.pipeline.weighted_pipeline_value) }}</strong></div>
              <div class="kv-row"><span>Stale leads / deals ({{ dash.parameters.stale_days }}d)</span><strong>{{ dash.pipeline.stale_leads }} / {{ dash.pipeline.stale_deals }}</strong></div>
              <div class="kv-row"><span>Overdue deals</span><strong>{{ dash.pipeline.overdue_deals_count }}</strong></div>
              <div class="kv-row"><span>Closing within {{ dash.parameters.closing_within_days }}d</span><strong>{{ dash.pipeline.closing_soon_count }}</strong></div>
            </div>
          </section>

          <section class="premium-card">
            <div class="card-header-row no-collapse">
              <div class="card-title-group"><h2>Lead Status Mix</h2></div>
            </div>
            <div class="card-content kv-list">
              <div v-for="s in dash.lead_status_counts" :key="s.key" class="kv-row">
                <span><Pill :type="leadStatusType(s.key)" :text="s.key.toUpperCase()" /></span>
                <strong>{{ s.count }}</strong>
              </div>
            </div>
          </section>

          <section class="premium-card">
            <div class="card-header-row no-collapse">
              <div class="card-title-group"><h2>Stage Rollup</h2></div>
            </div>
            <div class="card-content kv-list">
              <div v-for="s in dash.deal_stage_rollup" :key="s.stage" class="kv-row">
                <span>{{ s.stage }} ({{ s.count }})</span>
                <strong>{{ fmtTotals(s.total_value) }}</strong>
              </div>
            </div>
          </section>

          <section class="premium-card">
            <div class="card-header-row no-collapse">
              <div class="card-title-group"><h2>Closing Soon</h2></div>
              <span class="card-summary">{{ dash.closing_soon_deals.length }}</span>
            </div>
            <div v-if="!dash.closing_soon_deals.length" class="card-content empty-state">Nothing closing soon.</div>
            <div v-else class="card-content mini-list">
              <button v-for="d in dash.closing_soon_deals" :key="d.id" class="mini-row" @click="openDrawer('deal', d.id)">
                <span class="mini-title">{{ d.title }}</span>
                <span class="mini-meta">{{ fmtMoney(d.amount, d.currency) }} · {{ fmtDate(d.expected_close) }}</span>
              </button>
            </div>
          </section>

          <section class="premium-card">
            <div class="card-header-row no-collapse">
              <div class="card-title-group"><h2>Stale Deals</h2></div>
              <span class="card-summary">&gt; {{ dash.parameters.stale_days }}d untouched</span>
            </div>
            <div v-if="!dash.stale_deals.length" class="card-content empty-state">No stale deals.</div>
            <div v-else class="card-content mini-list">
              <button v-for="d in dash.stale_deals" :key="d.id" class="mini-row" @click="openDrawer('deal', d.id)">
                <span class="mini-title">{{ d.title }}</span>
                <span class="mini-meta">{{ d.stage }} · updated {{ timeAgo(d.updated_at) }}</span>
              </button>
            </div>
          </section>
        </div>
      </template>
    </template>

    <!-- ── Leads ─────────────────────────────────────────────────────────── -->
    <template v-else-if="tab === 'leads'">
      <div class="filter-bar">
        <input
          v-model="leadQ"
          class="premium-input search-input"
          placeholder="Search name, email, company, notes, tags…"
          @keyup.enter="loadLeads"
        />
        <select v-model="leadStatus" class="premium-input slim-select" @change="loadLeads">
          <option value="All">All statuses</option>
          <option v-for="s in LEAD_STATUSES" :key="s" :value="s">{{ s }}</option>
        </select>
        <span class="filter-count">{{ leadTotal }} lead(s)</span>
      </div>

      <section class="premium-card table-card">
        <div v-if="!leads.length" class="empty-state">No leads found. Create one or adjust the filter.</div>
        <div v-else class="table-scroll">
          <table class="crm-table">
            <thead>
              <tr>
                <th>Name</th><th>Email</th><th>Company</th><th>Status</th><th>Source</th><th>Updated</th><th></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="l in leads" :key="l.id">
                <td class="clickable strong" @click="openDrawer('lead', l.id)">{{ l.name }}</td>
                <td>{{ l.email || '—' }}</td>
                <td>{{ l.company || '—' }}</td>
                <td>
                  <select class="premium-input slim-select" :value="l.status" @change="setLeadStatus(l, $event.target.value)">
                    <option v-for="s in LEAD_STATUSES" :key="s" :value="s">{{ s }}</option>
                  </select>
                </td>
                <td>{{ l.source || '—' }}</td>
                <td class="muted-cell">{{ timeAgo(l.updated_at) }}</td>
                <td class="row-actions">
                  <button class="btn btn-sm btn-ghost" @click="openLeadEdit(l.id)">Edit</button>
                  <button class="btn btn-sm btn-danger" @click="archiveRecord('lead', l.id)">Archive</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>

    <!-- ── Deals kanban ──────────────────────────────────────────────────── -->
    <template v-else-if="tab === 'deals'">
      <div class="filter-bar">
        <input
          v-model="dealQ"
          class="premium-input search-input"
          placeholder="Search deal titles, notes, tags…"
          @keyup.enter="loadDeals"
        />
        <span class="filter-count">{{ dealTotal }} deal(s)</span>
      </div>

      <div class="kanban">
        <div v-for="stage in DEAL_STAGES" :key="stage" class="kanban-col">
          <div class="kanban-head">
            <span class="kanban-stage">{{ stage }}</span>
            <span class="kanban-meta">{{ stageMeta(stage).count }} · {{ fmtTotals(stageMeta(stage).total_value) }}</span>
          </div>
          <div class="kanban-cards">
            <div v-if="!dealsByStage[stage].length" class="kanban-empty">—</div>
            <div v-for="d in dealsByStage[stage]" :key="d.id" class="kanban-card" @click="openDrawer('deal', d.id)">
              <div class="kanban-title">{{ d.title }}</div>
              <div class="kanban-amount">{{ fmtMoney(d.amount, d.currency) }}</div>
              <div class="kanban-foot">
                <span v-if="d.expected_close" class="kanban-close">closes {{ fmtDate(d.expected_close) }}</span>
                <select
                  class="premium-input slim-select"
                  :value="d.stage"
                  @click.stop
                  @change="setDealStage(d, $event.target.value)"
                >
                  <option v-for="s in DEAL_STAGES" :key="s" :value="s">{{ s }}</option>
                </select>
              </div>
            </div>
          </div>
        </div>
      </div>
    </template>

    <!-- ── Organizations ─────────────────────────────────────────────────── -->
    <template v-else-if="tab === 'orgs'">
      <div class="filter-bar">
        <input
          v-model="orgQ"
          class="premium-input search-input"
          placeholder="Search name, industry, country, website…"
          @keyup.enter="loadOrgs"
        />
        <span class="filter-count">{{ orgTotal }} organization(s)</span>
      </div>

      <section class="premium-card table-card">
        <div v-if="!orgs.length" class="empty-state">No organizations found.</div>
        <div v-else class="table-scroll">
          <table class="crm-table">
            <thead>
              <tr><th>Name</th><th>Industry</th><th>Website</th><th>Email</th><th>Updated</th><th></th></tr>
            </thead>
            <tbody>
              <tr v-for="o in orgs" :key="o.id">
                <td class="clickable strong" @click="openDrawer('org', o.id)">{{ o.name }}</td>
                <td>{{ o.industry || '—' }}</td>
                <td>{{ o.website || '—' }}</td>
                <td>{{ o.email || '—' }}</td>
                <td class="muted-cell">{{ timeAgo(o.updated_at) }}</td>
                <td class="row-actions">
                  <button class="btn btn-sm btn-ghost" @click="openOrgEdit(o.id)">Edit</button>
                  <button class="btn btn-sm btn-danger" @click="archiveRecord('org', o.id)">Archive</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>

    <!-- ── Archived ──────────────────────────────────────────────────────── -->
    <template v-else-if="tab === 'archived'">
      <section class="premium-card table-card">
        <div v-if="!archived.length" class="empty-state">No archived records.</div>
        <div v-else class="table-scroll">
          <table class="crm-table">
            <thead>
              <tr><th>Type</th><th>Record</th><th>Archived</th><th></th></tr>
            </thead>
            <tbody>
              <tr v-for="a in archived" :key="`${a.entity_type}-${a.id}`">
                <td><Pill type="muted" :text="a.entity_type.toUpperCase()" /></td>
                <td class="strong">{{ a.label }}</td>
                <td class="muted-cell">{{ timeAgo(a.deleted_at) }}</td>
                <td class="row-actions">
                  <button class="btn btn-sm btn-save" @click="restoreRecord(a.entity_type, a.id)">Restore</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
  </div>

  <!-- ── Record drawer ───────────────────────────────────────────────────── -->
  <Teleport to="body">
    <Transition name="drawer-fade">
      <div v-if="drawer.open" class="drawer-overlay" @click.self="closeDrawer">
        <aside class="drawer">
          <div v-if="drawer.loading || !drawer.data" class="empty-state">Loading record…</div>
          <template v-else>
            <div class="drawer-head">
              <div class="drawer-title-group">
                <h2>{{ drawer.data.entity.name || drawer.data.entity.title }}</h2>
                <Pill
                  v-if="drawer.type === 'lead'"
                  :type="leadStatusType(drawer.data.entity.status)"
                  :text="drawer.data.entity.status.toUpperCase()"
                />
                <Pill
                  v-else-if="drawer.type === 'deal'"
                  :type="dealStageType(drawer.data.entity.stage)"
                  :text="drawer.data.entity.stage.toUpperCase()"
                />
                <Pill v-else type="info" text="ORGANIZATION" />
              </div>
              <div class="drawer-actions">
                <button v-if="drawer.type === 'lead'" class="btn btn-sm btn-primary" @click="openDealCreate(drawer.id)">New Deal</button>
                <button class="btn btn-sm btn-ghost" @click="editDrawerRecord">Edit</button>
                <button class="btn btn-sm btn-danger" @click="archiveRecord(drawer.type, drawer.id)">Archive</button>
                <button class="btn btn-sm btn-ghost" @click="closeDrawer">Close</button>
              </div>
            </div>

            <div class="drawer-body">
              <div class="kv-list drawer-fields">
                <template v-if="drawer.type === 'lead'">
                  <div class="kv-row"><span>Email</span><strong>{{ drawer.data.entity.email || '—' }}</strong></div>
                  <div class="kv-row"><span>Company</span><strong>{{ drawer.data.entity.company || '—' }}</strong></div>
                  <div class="kv-row"><span>Deals</span><strong>{{ drawer.data.summary.deal_count }}</strong></div>
                </template>
                <template v-else-if="drawer.type === 'deal'">
                  <div class="kv-row"><span>Amount</span><strong>{{ fmtMoney(drawer.data.entity.amount, drawer.data.entity.currency) }}</strong></div>
                  <div class="kv-row"><span>Probability</span><strong>{{ drawer.data.entity.probability ?? '—' }}<span v-if="drawer.data.entity.probability != null">%</span></strong></div>
                  <div class="kv-row"><span>Expected close</span><strong>{{ fmtDate(drawer.data.entity.expected_close) }}</strong></div>
                </template>
                <template v-else>
                  <div class="kv-row"><span>Industry</span><strong>{{ drawer.data.entity.industry || '—' }}</strong></div>
                  <div class="kv-row"><span>Website</span><strong>{{ drawer.data.entity.website || '—' }}</strong></div>
                  <div class="kv-row"><span>Leads / Deals</span><strong>{{ drawer.data.summary.lead_count }} / {{ drawer.data.summary.deal_count }}</strong></div>
                </template>
                <div class="kv-row"><span>Updated</span><strong>{{ timeAgo(drawer.data.entity.updated_at) }}</strong></div>
                <div v-if="(drawer.data.entity.tags || []).length" class="kv-row">
                  <span>Tags</span>
                  <span class="tag-list"><Pill v-for="t in drawer.data.entity.tags" :key="t" type="muted" :text="t" /></span>
                </div>
              </div>

              <div class="drawer-section">
                <h3>Linked Records</h3>
                <div class="mini-list">
                  <button
                    v-if="drawer.data.linked.organization"
                    class="mini-row"
                    @click="openDrawer('org', drawer.data.linked.organization.id)"
                  >
                    <span class="mini-title">🏢 {{ drawer.data.linked.organization.name }}</span>
                    <span class="mini-meta">organization</span>
                  </button>
                  <button
                    v-if="drawer.data.linked.lead"
                    class="mini-row"
                    @click="openDrawer('lead', drawer.data.linked.lead.id)"
                  >
                    <span class="mini-title">👤 {{ drawer.data.linked.lead.name }}</span>
                    <span class="mini-meta">{{ drawer.data.linked.lead.status }}</span>
                  </button>
                  <button
                    v-for="l in drawer.data.linked.leads || []"
                    :key="l.id"
                    class="mini-row"
                    @click="openDrawer('lead', l.id)"
                  >
                    <span class="mini-title">👤 {{ l.name }}</span>
                    <span class="mini-meta">{{ l.status }}</span>
                  </button>
                  <button
                    v-for="d in drawer.data.linked.deals || []"
                    :key="d.id"
                    class="mini-row"
                    @click="openDrawer('deal', d.id)"
                  >
                    <span class="mini-title">💼 {{ d.title }}</span>
                    <span class="mini-meta">{{ d.stage }} · {{ fmtMoney(d.amount, d.currency) }}</span>
                  </button>
                  <div
                    v-if="!drawer.data.linked.organization && !drawer.data.linked.lead && !(drawer.data.linked.leads || []).length && !(drawer.data.linked.deals || []).length"
                    class="empty-state slim"
                  >
                    No linked records.
                  </div>
                </div>
              </div>

              <div class="drawer-section">
                <h3>Activity Timeline</h3>
                <div v-if="!drawer.data.recent_activities.length" class="empty-state slim">No activity yet.</div>
                <div v-else class="timeline">
                  <div v-for="a in drawer.data.recent_activities" :key="a.id" class="timeline-item">
                    <div class="timeline-head">
                      <Pill type="info" :text="a.kind.toUpperCase()" />
                      <span class="timeline-title">{{ a.title }}</span>
                      <span class="timeline-time">{{ timeAgo(a.occurred_at) }}</span>
                    </div>
                    <p v-if="a.body" class="timeline-body">{{ a.body }}</p>
                  </div>
                </div>
              </div>

              <div class="drawer-section">
                <h3>Log Activity</h3>
                <div class="act-form">
                  <div class="act-form-row">
                    <select v-model="actForm.kind" class="premium-input slim-select">
                      <option v-for="k in ACTIVITY_KINDS" :key="k" :value="k">{{ k }}</option>
                    </select>
                    <input v-model="actForm.title" class="premium-input" placeholder="Short summary" @keyup.enter="logActivity" />
                  </div>
                  <textarea v-model="actForm.body" rows="2" class="premium-input textarea-input" placeholder="Details (optional)"></textarea>
                  <div class="act-form-actions">
                    <button class="btn btn-sm btn-save" @click="logActivity">Log</button>
                  </div>
                </div>
              </div>
            </div>
          </template>
        </aside>
      </div>
    </Transition>
  </Teleport>

  <!-- ── Lead modal ──────────────────────────────────────────────────────── -->
  <Modal v-model="leadModalOpen" :title="editingLeadId ? 'Edit Lead' : 'New Lead'" max-width="560px">
    <div class="form-container">
      <div class="form-grid">
        <div class="form-group-modern"><label>Name *</label><input v-model="leadForm.name" class="premium-input" /></div>
        <div class="form-group-modern"><label>Email</label><input v-model="leadForm.email" class="premium-input" /></div>
        <div class="form-group-modern"><label>Phone</label><input v-model="leadForm.phone" class="premium-input" /></div>
        <div class="form-group-modern"><label>Company</label><input v-model="leadForm.company" class="premium-input" /></div>
        <div class="form-group-modern">
          <label>Status</label>
          <select v-model="leadForm.status" class="premium-input select-input">
            <option v-for="s in LEAD_STATUSES" :key="s" :value="s">{{ s }}</option>
          </select>
        </div>
        <div class="form-group-modern"><label>Source</label><input v-model="leadForm.source" class="premium-input" placeholder="Website, Referral…" /></div>
      </div>
      <div class="form-group-modern"><label>Tags (comma-separated)</label><input v-model="leadForm.tags" class="premium-input" /></div>
      <div class="form-group-modern"><label>Notes</label><textarea v-model="leadForm.notes" rows="3" class="premium-input textarea-input"></textarea></div>
    </div>
    <div class="modal-actions-modern">
      <button class="btn btn-ghost" @click="leadModalOpen = false">Cancel</button>
      <button class="btn btn-save" @click="saveLead">{{ editingLeadId ? 'Save Changes' : 'Create Lead' }}</button>
    </div>
  </Modal>

  <!-- ── Deal modal ──────────────────────────────────────────────────────── -->
  <Modal v-model="dealModalOpen" :title="editingDealId ? 'Edit Deal' : 'New Deal'" max-width="560px">
    <div class="form-container">
      <div class="form-group-modern"><label>Title *</label><input v-model="dealForm.title" class="premium-input" /></div>
      <div class="form-grid">
        <div class="form-group-modern"><label>Amount</label><input v-model="dealForm.amount" type="number" min="0" step="0.01" class="premium-input" /></div>
        <div class="form-group-modern"><label>Currency</label><input v-model="dealForm.currency" class="premium-input" placeholder="USD" /></div>
        <div class="form-group-modern">
          <label>Stage</label>
          <select v-model="dealForm.stage" class="premium-input select-input">
            <option v-for="s in DEAL_STAGES" :key="s" :value="s">{{ s }}</option>
          </select>
        </div>
        <div class="form-group-modern"><label>Probability (%)</label><input v-model="dealForm.probability" type="number" min="0" max="100" class="premium-input" /></div>
        <div class="form-group-modern">
          <label>Contact (lead) {{ editingDealId ? '' : '*' }}</label>
          <select v-model="dealForm.contact_id" class="premium-input select-input">
            <option value="" disabled>Select a lead…</option>
            <option v-for="l in leads" :key="l.id" :value="l.id">{{ l.name }}{{ l.company ? ` — ${l.company}` : '' }}</option>
          </select>
        </div>
        <div class="form-group-modern"><label>Expected close</label><input v-model="dealForm.expected_close" type="date" class="premium-input" /></div>
      </div>
      <div class="form-group-modern"><label>Tags (comma-separated)</label><input v-model="dealForm.tags" class="premium-input" /></div>
      <div class="form-group-modern"><label>Notes</label><textarea v-model="dealForm.notes" rows="3" class="premium-input textarea-input"></textarea></div>
    </div>
    <div class="modal-actions-modern">
      <button class="btn btn-ghost" @click="dealModalOpen = false">Cancel</button>
      <button class="btn btn-save" @click="saveDeal">{{ editingDealId ? 'Save Changes' : 'Create Deal' }}</button>
    </div>
  </Modal>

  <!-- ── Org modal ───────────────────────────────────────────────────────── -->
  <Modal v-model="orgModalOpen" :title="editingOrgId ? 'Edit Organization' : 'New Organization'" max-width="560px">
    <div class="form-container">
      <div class="form-group-modern"><label>Name *</label><input v-model="orgForm.name" class="premium-input" /></div>
      <div class="form-grid">
        <div class="form-group-modern"><label>Website</label><input v-model="orgForm.website" class="premium-input" /></div>
        <div class="form-group-modern"><label>Industry</label><input v-model="orgForm.industry" class="premium-input" /></div>
        <div class="form-group-modern">
          <label>Size</label>
          <select v-model="orgForm.size" class="premium-input select-input">
            <option value="">—</option>
            <option v-for="s in ['1-10', '11-50', '51-200', '201-1000', '1000+']" :key="s" :value="s">{{ s }}</option>
          </select>
        </div>
        <div class="form-group-modern"><label>Country</label><input v-model="orgForm.country" class="premium-input" /></div>
        <div class="form-group-modern"><label>Phone</label><input v-model="orgForm.phone" class="premium-input" /></div>
        <div class="form-group-modern"><label>Email</label><input v-model="orgForm.email" class="premium-input" /></div>
      </div>
      <div class="form-group-modern"><label>Tags (comma-separated)</label><input v-model="orgForm.tags" class="premium-input" /></div>
      <div class="form-group-modern"><label>Notes</label><textarea v-model="orgForm.notes" rows="3" class="premium-input textarea-input"></textarea></div>
    </div>
    <div class="modal-actions-modern">
      <button class="btn btn-ghost" @click="orgModalOpen = false">Cancel</button>
      <button class="btn btn-save" @click="saveOrg">{{ editingOrgId ? 'Save Changes' : 'Create Organization' }}</button>
    </div>
  </Modal>
</template>

<style scoped>
.crm-page {
  padding-bottom: 60px;
}

/* Tabs */
.crm-tabs {
  display: flex;
  gap: 6px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}

.crm-tab {
  padding: 8px 16px;
  border-radius: 8px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
  font-size: 13px;
  font-weight: 700;
  cursor: pointer;
}

.crm-tab:hover {
  color: var(--text);
}

.crm-tab.active {
  background: rgba(94, 234, 212, 0.08);
  border-color: rgba(94, 234, 212, 0.25);
  color: var(--teal);
}

/* Dashboard */
.stat-tiles {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}

.stat-tile {
  padding: 18px 20px;
  text-align: left;
}

.stat-value {
  font-size: 28px;
  font-weight: 800;
  line-height: 1.1;
}

.stat-label {
  margin-top: 4px;
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}

.dash-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
  align-items: start;
}

.kv-list {
  display: flex;
  flex-direction: column;
}

.kv-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 9px 0;
  border-bottom: 1px solid rgba(128, 128, 128, 0.1);
  font-size: 13px;
}

.kv-row:last-child {
  border-bottom: none;
}

.kv-row > span:first-child {
  color: var(--muted);
}

.mini-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* Global .card-content zeroes top padding; give list bodies breathing room below the header. */
.card-content.mini-list {
  padding-top: 10px !important;
}

.mini-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 8px 10px;
  border: none;
  border-radius: 8px;
  background: rgba(128, 128, 128, 0.06);
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}

.mini-row:hover {
  background: rgba(128, 128, 128, 0.12);
}

.mini-title {
  font-size: 13px;
  font-weight: 600;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mini-meta {
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
}

/* Filter bar */
.filter-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 14px;
  flex-wrap: wrap;
}

.search-input {
  flex: 1;
  min-width: 220px;
  max-width: 420px;
}

.slim-select {
  width: auto;
  padding-top: 6px;
  padding-bottom: 6px;
  font-size: 13px;
}

.filter-count {
  font-size: 12px;
  color: var(--muted);
  font-weight: 600;
}

/* Tables */
.table-card {
  padding: 0;
  overflow: hidden;
}

.table-scroll {
  overflow-x: auto;
}

.crm-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.crm-table th {
  text-align: left;
  padding: 12px 14px;
  font-size: 11px;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
  border-bottom: 1px solid rgba(128, 128, 128, 0.15);
  white-space: nowrap;
}

.crm-table td {
  padding: 10px 14px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.08);
  vertical-align: middle;
}

.crm-table tr:last-child td {
  border-bottom: none;
}

.crm-table tr:hover td {
  background: rgba(128, 128, 128, 0.05);
}

.clickable {
  cursor: pointer;
}

.clickable:hover {
  color: var(--teal);
  text-decoration: underline;
}

.strong {
  font-weight: 700;
}

.muted-cell {
  color: var(--muted);
  white-space: nowrap;
}

.row-actions {
  display: flex;
  gap: 6px;
  justify-content: flex-end;
  white-space: nowrap;
}

/* Kanban */
.kanban {
  display: grid;
  grid-template-columns: repeat(6, minmax(180px, 1fr));
  gap: 10px;
  overflow-x: auto;
  align-items: start;
  padding-bottom: 8px;
}

.kanban-col {
  min-width: 180px;
  background: rgba(128, 128, 128, 0.05);
  border: 1px solid rgba(128, 128, 128, 0.1);
  border-radius: 12px;
  padding: 10px;
}

.kanban-head {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 2px 4px 10px;
}

.kanban-stage {
  font-size: 12px;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.kanban-meta {
  font-size: 11px;
  color: var(--muted);
}

.kanban-cards {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.kanban-empty {
  text-align: center;
  color: var(--muted);
  font-size: 12px;
  padding: 12px 0;
}

.kanban-card {
  background: rgba(128, 128, 128, 0.08);
  border: 1px solid rgba(128, 128, 128, 0.12);
  border-radius: 10px;
  padding: 10px;
  cursor: pointer;
}

.kanban-card:hover {
  border-color: rgba(94, 234, 212, 0.4);
}

.kanban-title {
  font-size: 13px;
  font-weight: 700;
  margin-bottom: 4px;
  word-break: break-word;
}

.kanban-amount {
  font-size: 12px;
  font-weight: 600;
  color: var(--teal);
  margin-bottom: 8px;
}

.kanban-foot {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.kanban-close {
  font-size: 11px;
  color: var(--muted);
}

/* Drawer */
.drawer-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  z-index: 90;
  display: flex;
  justify-content: flex-end;
}

.drawer {
  width: min(520px, 100vw);
  height: 100%;
  background: var(--bg, #111);
  border-left: 1px solid rgba(128, 128, 128, 0.2);
  display: flex;
  flex-direction: column;
  overflow-y: auto;
  padding: 20px;
}

.drawer-fade-enter-active,
.drawer-fade-leave-active {
  transition: opacity 0.2s ease;
}

.drawer-fade-enter-from,
.drawer-fade-leave-to {
  opacity: 0;
}

.drawer-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  padding-bottom: 14px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.15);
  flex-wrap: wrap;
}

.drawer-title-group {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex-wrap: wrap;
}

.drawer-title-group h2 {
  margin: 0;
  font-size: 18px;
}

.drawer-actions {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.drawer-body {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding-top: 14px;
}

.drawer-section h3 {
  margin: 0 0 8px;
  font-size: 12px;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
}

.tag-list {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.timeline {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.timeline-item {
  padding: 10px;
  border-radius: 8px;
  background: rgba(128, 128, 128, 0.06);
}

.timeline-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.timeline-title {
  font-size: 13px;
  font-weight: 700;
  flex: 1;
  min-width: 0;
}

.timeline-time {
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
}

.timeline-body {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--muted);
  white-space: pre-wrap;
  word-break: break-word;
}

.act-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.act-form-row {
  display: flex;
  gap: 8px;
}

.act-form-row input {
  flex: 1;
}

.act-form-actions {
  display: flex;
  justify-content: flex-end;
}

/* Forms / modals */
.form-container {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-bottom: 20px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}

.form-group-modern {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.form-group-modern label {
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
  color: var(--muted);
  letter-spacing: 0.05em;
}

.modal-actions-modern {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  padding-top: 16px;
  border-top: 1px solid rgba(128, 128, 128, 0.1);
}

.empty-state {
  padding: 40px;
  text-align: center;
  color: var(--muted);
  font-size: 14px;
}

.empty-state.slim {
  padding: 12px;
  font-size: 13px;
  text-align: left;
}

@media (max-width: 1100px) {
  .stat-tiles {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .dash-grid {
    grid-template-columns: 1fr;
  }

  .kanban {
    grid-template-columns: repeat(6, 220px);
  }
}

@media (max-width: 720px) {
  .form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
