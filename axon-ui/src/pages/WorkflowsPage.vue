<script setup>
import { ref, onMounted, onUnmounted, computed, watch, nextTick } from 'vue'
import { get, post, del } from '../lib/api.js'
import { toast } from '../lib/toast.js'
import { timeAgo, safeJsonParse } from '../lib/utils.js'
import WorkflowCanvas from '../components/WorkflowCanvas.vue'
import Pill from '../components/Pill.vue'
import NodeDetails from '../components/NodeDetails.vue'
import { NODE_TYPES } from '../lib/nodes.js'
import { useCanvasMapping, createNodeData, createEdgeData } from '../composables/useCanvasMapping.js'
import { NodeConnectionTypes } from '../lib/canvas/constants.js'
import { renameNodeInExpressions } from '../lib/expressionUpdates.js'

// Ensure a node label is unique across the canvas. Duplicate labels break
// $node["Name"] resolution (the resolver returns the first match) and
// collide in the backend $node map, so this is load-bearing for
// drag-and-drop variables. The first node of a name keeps it bare ("Axon");
// the next ones become "Axon 1", "Axon 2", ... until free.
function makeUniqueLabel(baseName, excludeId = null) {
  const taken = new Set(
    nodes.value
      .filter(n => n.id !== excludeId)
      .map(n => (n.data?.label || '').toLowerCase())
  )
  if (!taken.has(baseName.toLowerCase())) return baseName
  let counter = 1
  while (taken.has(`${baseName} ${counter}`.toLowerCase())) counter++
  return `${baseName} ${counter}`
}

// MCP Tools (loaded from API for dynamic MCP node properties)
const mcpTools = ref([])

const workflows = ref([])
const selectedWorkflow = ref(null)
const expandedRuns = ref({})
const collapsedRuns = ref({}) // runId (or index) -> boolean

// Current Editor State
const wfId = ref('')
const wfName = ref('')
const wfDesc = ref('')
// Error workflow (A3): id of the handler to run when THIS workflow fails. Null
// = use the global default. Edited via the toolbar settings popover.
const wfErrorWorkflowId = ref(null)
const showWfSettings = ref(false)
// Hidden file input used by the Import button (A5).
const importFileRef = ref(null)
const trigger = ref({ type: 'manual', config: {} })
const nodes = ref([])
const edges = ref([])
const selectedNode = ref(null)
const showHistory = ref(false)
const isNodeDetailsOpen = ref(false)
const lastRunResult = ref(null)
const pollTimer = ref(null)
const activeRunId = ref(null) // run_id currently executing — used to cancel only this run
const backendDone = ref(false) // Set true only after the last run result batch is received
const isNodePickerOpen = ref(false)
const nodeSearchQuery = ref('')
const isWorkflowMenuOpen = ref(false)
const workflowMenuSearch = ref('')
const isWorkflowSidebarCollapsed = ref(true)
const isCompactWorkflowLayout = ref(false)

const filteredNodeTypes = computed(() => {
  const query = nodeSearchQuery.value.trim().toLowerCase()
  const allNodes = Object.values(NODE_TYPES)
  if (!query) return allNodes
  return allNodes.filter(nt => 
    (nt.displayName || '').toLowerCase().includes(query) ||
    (nt.description || '').toLowerCase().includes(query) ||
    (nt.name || '').toLowerCase().includes(query)
  )
})
const pendingSplice = ref(null)
const pendingReplace = ref(null) // node being replaced via right-click → Replace
const nodePickerRef = ref(null)
const nodeSearchInputRef = ref(null)
const historyRef = ref(null)
const wfSettingsRef = ref(null)
const workflowMenuRef = ref(null)
const isExecuting = ref(false)
const contextMenuVisible = ref(false)
const contextMenuPos = ref({ x: 0, y: 0 })
const contextMenuNode = ref(null)
const contextMenuRef = ref(null)
const renamingNodeId = ref(null)
const pendingSource = ref(null) // { nodeId, handleId }

// Canvas ref for calling methods
const canvasRef = ref(null)

// Auto-focus the neuron search field whenever the node picker opens so the
// user can start typing a keyword immediately (n8n-style).
watch(isNodePickerOpen, (open) => {
  if (open) {
    nextTick(() => nodeSearchInputRef.value?.focus())
  }
})

const filteredWorkflows = computed(() => {
  const query = workflowMenuSearch.value.trim().toLowerCase()
  if (!query) return workflows.value
  return workflows.value.filter((wf) =>
    (wf.name || '').toLowerCase().includes(query) ||
    (wf.trigger_type || '').toLowerCase().includes(query)
  )
})

function toggleWorkflowMenu() {
  isWorkflowMenuOpen.value = !isWorkflowMenuOpen.value
}

function toggleWorkflowSidebar() {}

// Use canvas mapping for n8n-style data transformation
const { nodes: mappedNodes, edges: mappedEdges } = useCanvasMapping({
  nodes,
  edges,
  workflowObject: computed(() => selectedWorkflow.value),
})

const spreadsheetOptions = ref([])
const spreadsheetTabsById = ref({})
const spreadsheetTabIdMapsById = ref({})
const loadingSpreadsheetTabsById = ref({})
const calendarOptions = ref([])

async function loadSpreadsheets() {
  try {
    const d = await get('/google/sheets')
    const files = d.files || []
    spreadsheetOptions.value = files.map(f => ({ name: f.name, value: f.id }))
  } catch (e) {
    console.error('Failed to load Google Sheets', e)
  }
}

async function loadCalendars() {
  try {
    const d = await get('/google/calendars')
    const cals = d.calendars || []
    calendarOptions.value = [
      { name: 'Primary Calendar', value: 'primary' },
      ...cals.filter(c => c.value !== 'primary').map(c => ({ name: c.name, value: c.value }))
    ]
  } catch (e) {
    console.error('Failed to load Google Calendars', e)
    calendarOptions.value = [{ name: 'Primary Calendar', value: 'primary' }]
  }
}

function getSpreadsheetIdFromConfig(nodeConfig = {}) {
  return (
    nodeConfig?.spreadsheet_id ||
    nodeConfig?.source_spreadsheet_id ||
    nodeConfig?.destination_spreadsheet_id ||
    ''
  )
}

async function loadSpreadsheetTabs(spreadsheetId) {
  if (!spreadsheetId) return
  if (spreadsheetTabsById.value[spreadsheetId]) return
  if (loadingSpreadsheetTabsById.value[spreadsheetId]) return

  loadingSpreadsheetTabsById.value = {
    ...loadingSpreadsheetTabsById.value,
    [spreadsheetId]: true,
  }

  try {
    const encodedId = encodeURIComponent(spreadsheetId)
    const d = await get(`/google/sheets/${encodedId}/tabs`)
    const tabs = Array.isArray(d.tabs) ? d.tabs : []
    spreadsheetTabsById.value = {
      ...spreadsheetTabsById.value,
      [spreadsheetId]: tabs,
    }
    spreadsheetTabIdMapsById.value = {
      ...spreadsheetTabIdMapsById.value,
      [spreadsheetId]: d.sheet_id_map || {},
    }
  } catch (e) {
    console.error('Failed to load Google Sheet tabs', e)
  } finally {
    loadingSpreadsheetTabsById.value = {
      ...loadingSpreadsheetTabsById.value,
      [spreadsheetId]: false,
    }
  }
}

function getInitialConfig(type) {
  const def = NODE_TYPES[type]
  if (!def) return {}
  const config = {}
  def.properties.forEach(p => {
    // Clone object/array defaults (e.g. a fixedCollection's `rules`) so every
    // new node owns its own copy — otherwise two nodes of the same type share
    // one default object and editing one (e.g. adding a Switch rule) mutates
    // the other and the shared definition.
    const d = p.default
    config[p.name] = (d && typeof d === 'object') ? JSON.parse(JSON.stringify(d)) : d
  })
  return config
}

// ── MCP Tools Dynamic Schema ──────────────────────────────────────────────────
const MCP_SERVICE_META = {
  crm: { label: 'CRM', icon: '/icons/crm.png', color: '#7C3AED' },
  gsheets: { label: 'Google Sheets', icon: '/icons/google_sheets.png', color: '#0F9D58' },
  gmail: { label: 'Gmail', icon: '/icons/gmail.png', color: '#EA4335' },
  gdrive: { label: 'Google Drive', icon: '/icons/google_drive.png', color: '#4285F4' },
  gcal: { label: 'Google Calendar', icon: '/icons/google_calendar.png', color: '#4285F4' },
  gdocs: { label: 'Google Docs', icon: '/icons/google_docs.png', color: '#4285F4' },
  gslides: { label: 'Google Slides', icon: '/icons/slides.png', color: '#F4B400' },
  gchat: { label: 'Google Chat', icon: '💬', color: '#00897B' },
  gcon: { label: 'Google Contacts', icon: '/icons/google_contacts.png', color: '#4285F4' },
  gyoutube: { label: 'YouTube', icon: '/icons/youtube.png', color: '#FF0000' },
  gplaces: { label: 'Google Places', icon: '/icons/google_places.png', color: '#34A853' },
  gmeet: { label: 'Google Meet', icon: '/icons/google_meet.png', color: '#00897B' },
  fb: { label: 'Facebook', icon: '/icons/facebook.png', color: '#1877F2' },
  ig: { label: 'Instagram', icon: '/icons/instagram.png', color: '#E4405F' },
  outlook: { label: 'Outlook', icon: '/icons/outlook.png', color: '#0078D4' },
  mscal: { label: 'MS Calendar', icon: '/icons/ms_calendar.png', color: '#0078D4' },
  onedrive: { label: 'OneDrive', icon: '/icons/onedrive.png', color: '#0078D4' },
  mscontacts: { label: 'MS Contacts', icon: '/icons/outlook.png', color: '#0078D4' }
};

function schemaToProperties(tool, nodeConfig = {}) {
  const props = []
  const params = tool.parameters || {}
  const required = tool.required || []
  const spreadsheetIdKeys = new Set(['spreadsheet_id', 'source_spreadsheet_id', 'destination_spreadsheet_id'])
  const selectedSpreadsheetId = getSpreadsheetIdFromConfig(nodeConfig)
  const selectedSheetTabs = selectedSpreadsheetId ? (spreadsheetTabsById.value[selectedSpreadsheetId] || []) : []

  if (selectedSpreadsheetId && !spreadsheetTabsById.value[selectedSpreadsheetId]) {
    loadSpreadsheetTabs(selectedSpreadsheetId)
  }

  // ── Google Calendar specific ─────────────────────────────────────────────
  const RECURRENCE_LABELS = {
    '': 'Does not repeat',
    'RRULE:FREQ=DAILY': 'Every day',
    'RRULE:FREQ=WEEKLY': 'Every week',
    'RRULE:FREQ=MONTHLY': 'Every month',
    'RRULE:FREQ=YEARLY': 'Every year',
    'RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR': 'Every weekday (Mon–Fri)',
    'RRULE:FREQ=WEEKLY;BYDAY=MO': 'Every Monday',
    'RRULE:FREQ=WEEKLY;BYDAY=TU': 'Every Tuesday',
    'RRULE:FREQ=WEEKLY;BYDAY=WE': 'Every Wednesday',
    'RRULE:FREQ=WEEKLY;BYDAY=TH': 'Every Thursday',
    'RRULE:FREQ=WEEKLY;BYDAY=FR': 'Every Friday',
    'RRULE:FREQ=WEEKLY;BYDAY=SA': 'Every Saturday',
    'RRULE:FREQ=WEEKLY;BYDAY=SU': 'Every Sunday',
  }

  const isCalendarEvent = ['gcal_create_event', 'gcal_update_event'].includes(tool.name || tool.tool_name || '')

  for (const [key, schema] of Object.entries(params)) {
    const schemaEnum = Array.isArray(schema.enum) ? schema.enum : []
    const isSpreadsheetIdKey = spreadsheetIdKeys.has(key)

    const prop = {
      displayName: schema.title || key.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase()),
      name: key,
      default: schema.default !== undefined ? schema.default : (schema.type === 'boolean' ? false : ''),
      required: required.includes(key),
      hint: schema.description || '',
    }
    
    if (schema.displayOptions) {
      prop.displayOptions = schema.displayOptions
    }

    if (key === 'sheet_id' && selectedSheetTabs.length > 0) {
      prop.type = 'options'
      prop.searchable = true
      prop.options = selectedSheetTabs.map(tab => ({
        name: tab.title,
        value: tab.sheet_id,
        description: `Sheet ID: ${tab.sheet_id}`,
      }))
      const selectedMap = spreadsheetTabIdMapsById.value[selectedSpreadsheetId] || {}
      prop.hint = `Pick a sheet tab from the selected spreadsheet (${Object.keys(selectedMap).length} tabs loaded).`
    } else if (isSpreadsheetIdKey && spreadsheetOptions.value.length > 0) {
      prop.type = 'options'
      prop.searchable = true
      prop.options = spreadsheetOptions.value
    } else if (key === 'calendar_id' || key === 'source_calendar_id' || key === 'destination_calendar_id') {
      // Calendar dropdown — load from API or fall back to schemaEnum
      prop.type = 'options'
      prop.searchable = true
      prop.displayName = 'Calendar'
      prop.hint = 'Select which Google Calendar to use.'
      if (calendarOptions.value.length > 0) {
        prop.options = calendarOptions.value
      } else {
        // Load asynchonously and fall back to a generic primary option
        loadCalendars()
        prop.options = [{ name: 'Primary Calendar', value: 'primary' }]
      }
      prop.default = 'primary'
    } else if (key === 'recurrence' && isCalendarEvent) {
      // Recurrence dropdown with human-readable labels
      prop.type = 'options'
      prop.displayName = 'Recurrence'
      prop.hint = 'How often should this event repeat?'
      prop.options = Object.entries(RECURRENCE_LABELS).map(([value, name]) => ({ name, value }))
      prop.default = ''
    } else if (key === 'time_zone' && schemaEnum.length > 0) {
      // Timezone dropdown with search
      prop.type = 'options'
      prop.searchable = true
      prop.displayName = 'Time Zone'
      prop.hint = 'Timezone for the event start and end times.'
      prop.options = schemaEnum.map(v => ({ name: v, value: v }))
      prop.default = 'Asia/Manila'
    } else if (['start', 'end', 'time_min', 'time_max'].includes(key)) {
      prop.type = 'dateTime'
    } else if (schemaEnum.length > 0) {
      prop.type = 'options'
      prop.options = schemaEnum.map(v => ({ name: String(v), value: v }))
    } else if (schema.type === 'boolean') {
      prop.type = 'boolean'
    } else if (schema.type === 'number' || schema.type === 'integer') {
      prop.type = 'number'
    } else if (schema.type === 'array') {
      if (schema.items?.type === 'object' && schema.items?.properties) {
        prop.type = 'fixedCollection'
        prop.options = Object.entries(schema.items.properties).map(([subKey, subSchema]) => ({
          name: subKey,
          displayName: subKey.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase()),
          type: subSchema.type === 'boolean' ? 'boolean' : 'string',
          default: subSchema.type === 'boolean' ? false : '',
          placeholder: subSchema.description || ''
        }))
        const defaultParam = {}
        prop.options.forEach(o => defaultParam[o.name] = o.default)
        prop.default = { parameters: [defaultParam] }
        prop.hint = schema.description || 'Add items below'
      } else {
        prop.type = 'string'
        prop.typeOptions = { rows: 4 }
        const desc = schema.description ? schema.description + ' ' : ''
        prop.hint = `${desc}Enter one value, or a JSON array for multiple (e.g. ["a","b"]).`
        prop.placeholder = '["id1","id2"] or a single value'
        prop.default = ''
      }
    } else if (schema.type === 'object') {
      prop.type = 'string'
      prop.typeOptions = { rows: 6 }
      prop.hint = (schema.description || '') + ' (JSON object)'
      prop.default = ''
    } else {
      prop.type = 'string'
      // Multi-line for long text fields
      if (key === 'formula' || key === 'text' || key === 'body' || key === 'message' || key === 'query' || key === 'prompt' || key.endsWith('_json')) {
        prop.typeOptions = { rows: 4 }
      }
      if (key === 'upload_file_path') {
        prop.placeholder = 'C:\\path\\to\\your\\file.mp4'
      }
    }
    props.push(prop)
  }

  // Group properties that share an inlineGroup
  const finalProps = []
  const groupMap = {}

  for (const prop of props) {
    const groupId = prop.displayOptions?.inlineGroup
    if (groupId) {
      if (!groupMap[groupId]) {
        groupMap[groupId] = {
          name: groupId,
          type: 'inlineGroup',
          options: []
        }
        finalProps.push(groupMap[groupId])
      }
      groupMap[groupId].options.push(prop)
    } else {
      finalProps.push(prop)
    }
  }

  return finalProps
}

function formatToolActionLabel(tool) {
  const rawName = String(tool?.tool_name || tool?.name || '')
  const rawDesc = String(tool?.description || '').trim()
  if (rawDesc.length > 0) return rawDesc

  const bare = rawName.split(/[.:/]/).pop() || rawName
  const human = bare.replace(/_/g, ' ')
  return human.charAt(0).toUpperCase() + human.slice(1)
}

async function loadMcpTools() {
  try {
    const d = await get('/mcp/tools')
    mcpTools.value = (d.tools || []).filter(t => t.name !== 'image_tool')
    
    // Group tools by service prefix
    const groupedTools = {}
    const serviceKeys = Object.keys(MCP_SERVICE_META)
    for (const t of mcpTools.value) {
      // Prefer MCP-native tool_name when available; fall back to registry name.
      const rawToolName = String(t.tool_name || t.name || '').toLowerCase()
      const bareToolName = rawToolName.split(/[.:/]/).pop() || rawToolName
      let prefix = bareToolName.split('_')[0]

      // Namespace-safe detection (e.g., server.gyoutube_* or server:gyoutube_*).
      const knownPrefix = serviceKeys.find((k) =>
        rawToolName === k ||
        rawToolName.startsWith(`${k}_`) ||
        rawToolName.includes(`:${k}_`) ||
        rawToolName.includes(`.${k}_`) ||
        rawToolName.includes(`/${k}_`)
      )
      if (knownPrefix) {
        prefix = knownPrefix
      } else if (!MCP_SERVICE_META[prefix]) {
        prefix = t.server && t.server !== 'unknown' ? t.server : 'mcp_generic'
      }
      
      if (!groupedTools[prefix]) groupedTools[prefix] = []
      groupedTools[prefix].push(t)
    }

    // Now inject each group into NODE_TYPES
    for (const [prefix, tools] of Object.entries(groupedTools)) {
      const meta = MCP_SERVICE_META[prefix] || { label: prefix, icon: '🔌' }
      const nodeName = prefix.startsWith('mcp_') ? prefix : `mcp_${prefix}`
      
      NODE_TYPES[nodeName] = {
        displayName: meta.label + ' Tool',
        name: nodeName,
        icon: meta.icon,
        description: `Execute actions from ${meta.label}`,
        properties: [
            {
                displayName: 'Tool Action',
                name: 'tool_name',
                type: 'options',
                // Keep value as registry tool name (execution-safe), but display MCP-native label.
                options: tools.map(t => ({ name: formatToolActionLabel(t), value: t.name, description: t.tool_name || t.name })),
                searchable: true,
                default: tools.length > 0 ? tools[0].name : '',
                required: true,
                hint: `Select an action for ${meta.label}`,
            },
        ],
        dynamic: true,
      }
    }
    
    // Remove the old 'mcp' monolithic node type if it exists in nodes.js
    if (NODE_TYPES.mcp) delete NODE_TYPES.mcp

  } catch (e) {
    console.warn('MCP tools load failed:', e)
  }
}

// Computed: returns dynamic properties for the currently-selected MCP tool
function getMcpDynamicProps(toolName) {
  if (!toolName) return []
  const tool = mcpTools.value.find(t => t.name === toolName)
  if (!tool) return []
  const nodeConfig = selectedNode.value?.data?.config || {}
  return schemaToProperties(tool, nodeConfig)
}

watch(
  () => [
    selectedNode.value?.data?.config?.spreadsheet_id,
    selectedNode.value?.data?.config?.source_spreadsheet_id,
    selectedNode.value?.data?.config?.destination_spreadsheet_id,
  ],
  ([spreadsheetId, sourceSpreadsheetId, destinationSpreadsheetId]) => {
    if (spreadsheetId) loadSpreadsheetTabs(spreadsheetId)
    if (sourceSpreadsheetId && sourceSpreadsheetId !== spreadsheetId) {
      loadSpreadsheetTabs(sourceSpreadsheetId)
    }
    if (
      destinationSpreadsheetId &&
      destinationSpreadsheetId !== spreadsheetId &&
      destinationSpreadsheetId !== sourceSpreadsheetId
    ) {
      loadSpreadsheetTabs(destinationSpreadsheetId)
    }
  },
  { immediate: true }
)

async function load() {
  try {
    const d = await get('/workflows')
    workflows.value = d.workflows || []
    if (workflows.value.length > 0 && !selectedWorkflow.value) {
      selectWorkflow(workflows.value[0])
    }
  } catch (e) {
    toast('Failed to load workflows', false)
  }
}

// Backward-compat: legacy Switch nodes always exposed a fixed Default output at
// index 5 (Case1..Case5 + Default). Dynamic switches put Default right after the
// last rule (index = rule count), so remap any stale default edge on load. Only
// touches edges whose source is a Switch and whose handle is the old default.
function migrateSwitchDefaultEdges(nodeList, edgeList) {
  const ruleCountBySwitch = new Map()
  for (const n of nodeList) {
    if (n.data?.node_type !== 'switch') continue
    const rules = n.data?.config?.rules?.parameters
    ruleCountBySwitch.set(n.id, Array.isArray(rules) ? rules.length : 0)
  }
  if (ruleCountBySwitch.size === 0) return

  for (const e of edgeList) {
    if (e.sourceHandle !== 'output_main_5') continue
    if (!ruleCountBySwitch.has(e.source)) continue
    const ruleCount = ruleCountBySwitch.get(e.source)
    if (ruleCount === 5) continue // already the correct default index
    const newHandle = `output_main_${ruleCount}`
    e.sourceHandle = newHandle
    if (e.data) e.data.sourceHandle = newHandle
  }
}

// Dynamic outputs (Switch rules) are positional: handles are output_main_<index>,
// with rules 0..N-1 followed by a trailing Default at N. Adding or removing a rule
// shifts every later index, so an edge stored under the old index would silently
// re-target the wrong rule. These remap edges from a given source so each wire
// stays on its rule — or is dropped when its rule is deleted.
// `mutate(outIdx)` returns: a new index to move the edge, `null` to delete it, or
// `undefined` to leave it untouched.
function remapDynamicOutputEdges(nodeId, mutate) {
  const next = []
  for (const e of edges.value) {
    if (e.source !== nodeId) { next.push(e); continue }
    const m = /^output_(.+)_(\d+)$/.exec(e.sourceHandle || '')
    if (!m) { next.push(e); continue }
    const result = mutate(parseInt(m[2], 10))
    if (result === null) continue // rule deleted → drop its edge
    if (result !== undefined) {
      const newHandle = `output_${m[1]}_${result}`
      e.sourceHandle = newHandle
      if (e.data) e.data.sourceHandle = newHandle
    }
    next.push(e)
  }
  edges.value = next
}

function handleDynamicOutputRemoved({ nodeId, index }) {
  remapDynamicOutputEdges(nodeId, (outIdx) => {
    if (outIdx === index) return null     // the deleted rule's own edge
    if (outIdx > index) return outIdx - 1 // later outputs shift up one slot
    return undefined
  })
}

function handleDynamicOutputAdded({ nodeId, index }) {
  remapDynamicOutputEdges(nodeId, (outIdx) => {
    if (outIdx >= index) return outIdx + 1 // Default (and beyond) shift down one
    return undefined
  })
}

async function selectWorkflow(wf) {
  selectedWorkflow.value = wf
  isWorkflowMenuOpen.value = false
  workflowMenuSearch.value = ''
  wfId.value = wf.id
  wfName.value = wf.name
  wfDesc.value = wf.description || ''
  wfErrorWorkflowId.value = wf.error_workflow_id || null
  trigger.value = {
    type: wf.trigger_type || 'manual',
    config: { ...(wf.trigger_config || {}) },
  }

  // Map backend nodes to n8n-style Vue Flow nodes
  nodes.value = (wf.nodes || []).map((n) => ({
    id: n.id,
    type: 'canvas-node',
    position: { x: n.position_x || 0, y: n.position_y || 0 },
    data: { ...createNodeData(n.id, n.node_type, n.name, n.config, n.enabled !== false), continueOnFail: n.continue_on_fail === true, retries: n.retries || 0, retryWaitMs: n.retry_wait_ms || 0, retryBackoff: n.retry_backoff || 'fixed', pinnedData: n.pinned_data ?? null },
  }))

  // Map backend edges to Vue Flow edges with proper data
  edges.value = (wf.edges || []).map((e) => ({
    id: e.id,
    source: e.source_id,
    target: e.target_id,
    sourceHandle: e.source_handle || `output_${NodeConnectionTypes.Main}_0`,
    targetHandle: e.target_handle || `input_${NodeConnectionTypes.Main}_0`,
    type: 'canvas-edge',
    data: createEdgeData(
      e.id,
      e.source_id,
      e.target_id,
      e.source_handle || `output_${NodeConnectionTypes.Main}_0`,
      e.target_handle || `input_${NodeConnectionTypes.Main}_0`
    ),
  }))

  migrateSwitchDefaultEdges(nodes.value, edges.value)

  selectedNode.value = null
  showHistory.value = false
  isNodeDetailsOpen.value = false

  // Load latest run result automatically
  try {
    const runs = await get(`/workflows/${wf.id}/runs`)
    if (runs && runs.length > 0) {
      // Initialize with the most recent run
      lastRunResult.value = { ...runs[0], node_results: [...(runs[0].node_results || [])] }
    } else {
      lastRunResult.value = { node_results: [] }
    }
  } catch (e) {
    lastRunResult.value = { node_results: [] }
  }
}

function createNew() {
  isWorkflowMenuOpen.value = false
  workflowMenuSearch.value = ''
  wfId.value = ''
  wfName.value = 'New Workflow'
  wfDesc.value = ''
  wfErrorWorkflowId.value = null
  trigger.value = { type: 'manual', config: {} }
  nodes.value = []
  edges.value = []
  selectedWorkflow.value = { id: 'new', name: 'New Workflow' }
  selectedNode.value = null
}

// Workflows eligible as THIS workflow's failure handler (A3): every other
// saved workflow. A workflow can't be its own error handler.
const errorHandlerOptions = computed(() =>
  (workflows.value || []).filter((w) => w.id !== wfId.value)
)

// Export the current workflow as a downloadable JSON bundle (A5). Secrets never
// leave the box — node configs carry only credential_id references.
async function exportWorkflow() {
  if (!wfId.value || wfId.value === 'new') { toast('Save the workflow first', false); return }
  try {
    const bundle = await get(`/workflows/${wfId.value}/export`)
    if (bundle && bundle.ok === false) { toast(bundle.error || 'Export failed', false); return }
    const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    const safe = (wfName.value || 'workflow').replace(/[^\w.-]+/g, '_')
    a.download = `${safe}.axon.json`
    document.body.appendChild(a)
    a.click()
    a.remove()
    URL.revokeObjectURL(url)
    showWfSettings.value = false
  } catch (e) {
    toast('Export failed', false)
  }
}

function triggerImport() {
  importFileRef.value?.click()
}

// Import a bundle (A5): parse the file and POST it. The new workflow lands
// disabled so an imported trigger can't fire before review.
async function importWorkflow(event) {
  const file = event.target.files?.[0]
  event.target.value = '' // allow re-importing the same file
  if (!file) return
  try {
    const text = await file.text()
    const bundle = JSON.parse(text)
    const res = await post('/workflows/import', bundle)
    if (!res || res.ok === false || !res.id) { toast(res?.error || 'Import failed', false); return }
    await load()
    const imported = (workflows.value || []).find((w) => w.id === res.id)
    if (imported) selectWorkflow(imported)
    showWfSettings.value = false
    const credMsg = res.credentials_required ? ` — review ${res.credentials_required} credential(s)` : ''
    toast(`Imported "${res.name}" (disabled)${credMsg}`)
  } catch (e) {
    toast('Import failed — invalid bundle', false)
  }
}

// `opts.save` (default true) lets composite operations (splice / add-from-handle)
// add the node WITHOUT triggering an immediate save, so they can push the
// accompanying edges and persist everything in a single atomic POST. Saving
// here while the caller is still mutating edges fires a second, concurrent
// /workflows request whose DELETE+INSERT of edges races the caller's save —
// the source of the duplicated / mis-routed wires after inserting a node.
function addNode(type, position = { x: 250, y: 150 }, { save: shouldSave = true } = {}) {
  const id = `node_${Date.now()}_${Math.random().toString(36).substr(2, 5)}`
  const isTrigger = type === 'trigger' || nodes.value.length === 0
  const baseName = isTrigger
    ? 'When clicked'
    : (NODE_TYPES[type]?.displayName || 'Neuron')
  // First node of a type keeps the bare name ("Axon"); the next ones become
  // "Axon 1", "Axon 2", ... makeUniqueLabel also guards against post-delete
  // collisions (e.g. delete "Axon 1" then add another would reuse "Axon 1").
  const displayName = makeUniqueLabel(baseName, id)

  const newNode = {
    id,
    type: 'canvas-node',
    position,
    data: createNodeData(id, type, displayName, getInitialConfig(type)),
  }

  nodes.value.push(newNode)
  isNodePickerOpen.value = false
  if (shouldSave) save()
  return id
}

function onPaletteDragStart(e, type) {
  e.dataTransfer.setData('application/axon-node', type)
  e.dataTransfer.effectAllowed = 'move'
}

function handleAddNode({ type, position }) {
  addNode(type, position)
}

// Where to drop a node added via the toolbar "+" picker (no source handle, edge,
// or replace context). Land at the last spot the user *clicked* on the canvas, so
// the node appears exactly where they pointed — not at the end of the chain (which
// forced a viewport pan that shoved existing nodes aside), and not under the cursor's
// last hover, which drifts onto the "+" button as the mouse travels to click it.
// Falls back to right of the last node, then a fixed default, if nothing's been clicked.
function computeNewNodePosition() {
  const click = canvasRef.value?.getLastClickPosition?.()
  if (click) return { x: click.x, y: click.y }

  const list = nodes.value
  if (list.length === 0) return { x: 250, y: 150 }

  const NODE_W = 120 // rendered node width (matches the splice-insert spacing)
  const NODE_H = 140 // collision box height
  const GAP = 80
  // Collision box must be narrower than the placement offset (NODE_W + GAP = 200),
  // or the new node would "overlap" the rightmost node it's meant to sit beside and
  // get bumped down on every check.
  const COLLISION_W = 150
  // Right-most node = end of the chain. Sit just to its right, not a screen away.
  const rightmost = list.reduce((a, b) => (b.position.x > a.position.x ? b : a))
  const pos = { x: rightmost.position.x + NODE_W + GAP, y: rightmost.position.y }
  const overlaps = (p) => list.some((n) =>
    Math.abs(n.position.x - p.x) < COLLISION_W && Math.abs(n.position.y - p.y) < NODE_H)
  let guard = 0
  while (overlaps(pos) && guard++ < 100) pos.y += NODE_H
  return pos
}

function addNodeFromPalette(type) {
  if (pendingReplace.value) {
    replaceNode(pendingReplace.value.id, type)
    pendingReplace.value = null
    isNodePickerOpen.value = false
    return
  }

  if (pendingSource.value && canvasRef.value) {
    const sNode = nodes.value.find(n => n.id === pendingSource.value.nodeId)
    if (sNode) {
      // save:false — the connecting edge is pushed below; save once afterwards
      // so the node and its edge persist in a single, non-racing POST.
      const newNodeId = addNode(type, { x: sNode.position.x + 250, y: sNode.position.y }, { save: false })
      const edgeId = `e-${pendingSource.value.nodeId}-${newNodeId}`
      const sourceId = pendingSource.value.nodeId
      const sourceHandle = pendingSource.value.handleId
      const targetHandle = 'input_main_0'
      edges.value.push({
        id: edgeId,
        source: sourceId,
        target: newNodeId,
        sourceHandle,
        targetHandle,
        type: 'canvas-edge',
        data: createEdgeData(edgeId, sourceId, newNodeId, sourceHandle, targetHandle),
      })
      pendingSource.value = null
      save()
      isNodePickerOpen.value = false
      return
    }
  }

  if (pendingSplice.value) {
    const { edge } = pendingSplice.value
    const sourceNode = nodes.value.find(n => n.id === edge.source)
    const targetNode = nodes.value.find(n => n.id === edge.target)
    
    if (sourceNode && targetNode) {
      // Logic: Source [gap] NewNode [gap] Target
      const nodeWidth = 120
      const gap = 100
      
      const newNodeX = sourceNode.position.x + 120 + gap // sourceWidth + gap
      const targetMinX = newNodeX + nodeWidth + gap
      const shift = targetMinX - targetNode.position.x
      
      if (shift > 0) {
        const affectedNodes = getDownstreamNodes(edge.source)
        affectedNodes.forEach(n => {
          n.position.x += shift
        })
      }
      
      handleSpliceNode({ 
        type, 
        position: { x: newNodeX, y: sourceNode.position.y }, 
        edge 
      })
    }
    pendingSplice.value = null
  } else {
    // Plain "+" add: drop under the last canvas cursor position (paste-style), so
    // it lands where the user is looking — already on-screen, so no viewport pan.
    const position = computeNewNodePosition()
    addNode(type, position)
  }
  isNodePickerOpen.value = false
}

function handleSpliceNode({ type, position, edge }) {
  // save:false — the two replacement edges are created below; the single save()
  // at the end of this function persists the node + both edges atomically.
  // Letting addNode save here would race that save and leave the old edge
  // behind (duplicate wire) or drop the new edges (mis-routed wire).
  const newNodeId = addNode(type, position, { save: false })

  // Splicing: replace 1 edge with 2
  const sourceId = edge.source
  const targetId = edge.target
  const sourceHandle = edge.sourceHandle || `output_${NodeConnectionTypes.Main}_0`
  const targetHandle = edge.targetHandle || `input_${NodeConnectionTypes.Main}_0`

  // Remove the old edge
  edges.value = edges.value.filter((e) => e.id !== edge.id)

  // Create two new edges
  const edge1Id = `edge_${Date.now()}_1`
  const edge2Id = `edge_${Date.now()}_2`

  edges.value.push({
    id: edge1Id,
    type: 'canvas-edge',
    source: sourceId,
    target: newNodeId,
    sourceHandle,
    targetHandle: `input_${NodeConnectionTypes.Main}_0`,
    data: createEdgeData(edge1Id, sourceId, newNodeId, sourceHandle, `input_${NodeConnectionTypes.Main}_0`),
  })

  edges.value.push({
    id: edge2Id,
    type: 'canvas-edge',
    source: newNodeId,
    target: targetId,
    sourceHandle: `output_${NodeConnectionTypes.Main}_0`,
    targetHandle,
    data: createEdgeData(edge2Id, newNodeId, targetId, `output_${NodeConnectionTypes.Main}_0`, targetHandle),
  })

  save()
}

function handleInsertNode({ edgeId, position }) {
  const edge = edges.value.find((e) => e.id === edgeId)
  if (!edge) return
  
  pendingSplice.value = { edge, position }
  isNodePickerOpen.value = true
}

function handleHandleAdd({ nodeId, handleId }) {
  console.log('[Flow] Add from handle:', nodeId, handleId)
  pendingSource.value = { nodeId, handleId }
  isNodePickerOpen.value = true
}

function handleNodeSelect(node) {
  // Resolve to the source-of-truth node in nodes.value. Vue Flow passes its
  // own internal node clone through these events; storing that clone in
  // selectedNode would make NodeDetails edit a copy that save()/getWorkflowPayload
  // never reads, so renames and config changes silently never persist.
  selectedNode.value = resolveSelectedNode(node)
}

function handleNodeActivate(node) {
  const resolved = resolveSelectedNode(node)
  selectedNode.value = resolved
  isNodeDetailsOpen.value = !!resolved
}

// Map any node-shaped object (Vue Flow clone or source node) back to the
// corresponding entry in nodes.value, which is the array save() serializes.
function resolveSelectedNode(node) {
  if (!node) return null
  const id = typeof node === 'string' ? node : node.id
  return nodes.value.find(n => n.id === id) || node
}

// Toggle a node's enabled state from the canvas toolbar. Previously the toolbar
// emitted an event that no handler listened to, so the eye button did nothing.
// Keep `enabled` and `disabled` (used by CanvasNodeDefault for the strike-through)
// in sync, then persist.
function handleToggleNodeEnabled(nodeId) {
  const node = nodes.value.find(n => n.id === nodeId)
  if (!node) return
  const nextEnabled = node.data?.enabled === false
  node.data = {
    ...node.data,
    enabled: nextEnabled,
    disabled: !nextEnabled,
  }
  if (selectedNode.value && selectedNode.value.id === nodeId) {
    selectedNode.value = node
  }
  save()
}

function closeNodeDetails() {
  selectedNode.value = null
  isNodeDetailsOpen.value = false
}

function handleNodeDeselect() {
  closeNodeDetails()
}

// A node counts as the workflow entry point if it's a Stimulus/trigger node.
// Used to decide whether "Run" executes just that node (executeNodeStep) or
// kicks off the whole workflow (runActive). Centralized here because the same
// check was duplicated (and inconsistently) in runActive, getWorkflowPayload,
// and handleRunNode — the Stimulus starting node was missed in handleRunNode,
// so clicking run on it only stepped the node.
function isWorkflowEntryNode(node) {
  if (!node) return false
  const type = node.data?.node_type || node.data?.type
  return type === 'trigger' || type === 'circadian' || type === 'stimulus'
}

function handleRunNode(nodeId) {
  const node = nodes.value.find(n => n.id === nodeId)
  if (node && isWorkflowEntryNode(node)) {
    runActive()
  } else {
    executeNodeStep(nodeId)
  }
}

// True when every immediate upstream node of `nodeId` has REUSABLE output data
// from the last run. Used to decide whether "Execute Step" can run just this node
// (reusing that cached data) instead of re-running the whole chain.
//
// A parent only provides reusable data when it produced output AND did not error
// (`!!r.error` is the canonical error signal, same as updateNodeExecutionStates).
// If any immediate parent errored or has no output, fall back to re-running the
// whole chain so this node receives fresh, valid inputs. This matches NodeDetails'
// "Has Data" badge (getUpstreamData), which is gated on the same two conditions —
// so what the panel shows and what Execute Step does stay consistent.
function immediateUpstreamHaveData(nodeId) {
  const parentIds = edges.value
    .filter(e => edgeTarget(e) === nodeId)
    .map(e => edgeSource(e))
  if (parentIds.length === 0) return true // no upstream → nothing to wait on
  const haveData = new Set(
    (lastRunResult.value?.node_results || [])
      .filter(r => !!r.output && !r.error)
      .map(r => String(r.node_id))
  )
  return parentIds.every(pid => haveData.has(String(pid)))
}

async function executeNodeStep(nodeId, opts = {}) {
  if (!wfId.value || wfId.value === 'new') {
    toast('Save workflow first to execute steps', false)
    return
  }

  // "Execute Step" (opts.single) runs ONLY this node using the data its upstream
  // nodes already produced. If that upstream data isn't there yet, fall back to
  // running the full chain so the node still receives fresh inputs. The play
  // button never sets opts.single, so it always runs the chain.
  const single = !!opts.single && immediateUpstreamHaveData(nodeId)

  isExecuting.value = true
  toast(single ? `Executing ${nodeId} (step only)...` : `Executing ${nodeId}...`, true)

  // IMMEDIATE FEEDBACK: Start spinning the node being executed.
  // Note: waiting:false here is intentional — this node is directly clicked, not
  // a downstream node waiting on a predecessor. Using waiting:true would incorrectly
  // animate the incoming edge as if data were flowing into it from upstream.
  if (canvasRef.value) {
    canvasRef.value.updateNodeExecution(nodeId, {
      running: true,
      waiting: false,
      status: 'running',
    })
  }
  
  try {
    // Ensure current UI state is synced to DB before execution
    await save()
    
    backendDone.value = false

    // CRITICAL: Flush any stale results from a previous run before starting polling.
    // Without this, runLivePlayback reads old node_results, exhausts processedCount
    // against them, then exits immediately when the new (shorter) run completes —
    // leaving the clicked node stuck in the running:true state forever.
    // In single mode keep the other nodes' data on screen and clear only this
    // node's slot, so upstream panels stay populated while its fresh result plays.
    if (single) {
      lastRunResult.value = {
        ...(lastRunResult.value || {}),
        node_results: (lastRunResult.value?.node_results || []).filter(
          rr => String(rr.node_id) !== String(nodeId)
        ),
      }
    } else {
      lastRunResult.value = { node_results: [] }
    }

    // SELECTIVE EXECUTION: run this node alone (single) using cached upstream
    // data, or this node plus its dependencies (the chain up to it).
    const r = await post(`/workflows/${wfId.value}/run/${nodeId}${single ? '?single=true' : ''}`)
    if (r.ok) {
      console.log('[Flow] Step execution started:', r.run_id)
      startPolling(r.run_id)

      // TRIGGER LIVE PLAYBACK. In single mode, animate ONLY the clicked node even
      // though the run record carries the whole (merged) chain — so the rest of
      // the workflow doesn't replay.
      if (canvasRef.value) {
        canvasRef.value.runLivePlayback(
          () => {
            const all = lastRunResult.value?.node_results || []
            return single ? all.filter(rr => String(rr.node_id) === String(nodeId)) : all
          },
          () => isExecuting.value,
          () => backendDone.value
        ).then(() => {
          // Cleanup: reset any nodes that are still stuck in running/waiting state.
          // This catches nodes that were marked waiting:true mid-run but whose results
          // never arrived (e.g. skipped IF branches, or nodes after a stop-on-error halt).
          if (canvasRef.value) {
            nodes.value.forEach(n => {
              const exec = n.data?.execution
              if (exec && (exec.running || exec.waiting)) {
                canvasRef.value.updateNodeExecution(n.id, {
                  running: false,
                  waiting: false,
                  status: (exec.status === 'running' || exec.status === 'unknown') ? 'unknown' : exec.status,
                })
              }
            })
          }
          isExecuting.value = false
          stopPolling()
          toast('Step execution complete', true)
        })
      }
    } else {
      isExecuting.value = false
      stopPolling()
      toast(r.error || 'Execution failed', false)
      updateNodeExecutionStates([{ error: 'Failed' }]) 
    }
  } catch (e) {
    isExecuting.value = false
    stopPolling()
    toast('Execution error', false)
  }
}

function clearNodeExecution(nodeId) {
  // 1. Remove from lastRunResult.node_results
  if (lastRunResult.value && lastRunResult.value.node_results) {
    lastRunResult.value.node_results = lastRunResult.value.node_results.filter(
      r => String(r.node_id) !== String(nodeId)
    )
  }
  
  // 2. Clear from nodes.value
  const nodeIndex = nodes.value.findIndex(n => n.id === nodeId)
  if (nodeIndex !== -1) {
    nodes.value[nodeIndex].data.execution = null
  }
  
  toast('Execution data cleared', true)
}

async function stopWorkflow() {
  if (!wfId.value) return
  try {
    // Cancel only the active run so we don't poison future runs of this workflow.
    const q = activeRunId.value ? `?run_id=${encodeURIComponent(activeRunId.value)}` : ''
    await post(`/workflows/${wfId.value}/stop${q}`)
    toast('Stop requested', true)
    isExecuting.value = false
    stopPolling()
  } catch (e) {
    toast('Failed to stop workflow', false)
  }
}

async function runActive() {
  if (!wfId.value || wfId.value === 'new') return toast('Save workflow first', false)

  isExecuting.value = true

  // Reset visuals via batch update
  if (canvasRef.value) {
    const executionMap = {}
    nodes.value.forEach(n => {
      executionMap[n.id] = { running: false, waiting: false, status: 'unknown' }
    })
    canvasRef.value.updateAllNodesExecution(executionMap)
  }

  backendDone.value = false

  // CRITICAL: Flush stale results from any previous run before polling begins.
  // Same race condition as in executeNodeStep: old node_results would be consumed
  // immediately by runLivePlayback, causing it to exit before the new run's data arrives.
  lastRunResult.value = { node_results: [] }

  toast('Running workflow...', true)

  // IMMEDIATE FEEDBACK: Start spinning the trigger node immediately.
  // waiting:false on the trigger itself — it has no predecessor to receive data from.
  // Its direct children get waiting:true so their incoming edges animate right away.
  if (canvasRef.value) {
    const triggerNode = nodes.value.find(n => 
      n.data?.node_type === 'trigger' || n.data?.node_type === 'circadian' || n.data?.node_type === 'stimulus' ||
      n.data?.type === 'trigger' || n.data?.type === 'circadian' || n.data?.type === 'stimulus'
    )
    const tid = triggerNode ? triggerNode.id : (nodes.value[0]?.id)
    if (tid) {
      canvasRef.value.updateNodeExecution(tid, {
        running: true,
        waiting: false,
        status: 'running',
      })
      
      // Start "flowing wires" from trigger immediately
      const childrenIds = edges.value
        .filter(e => (typeof e.source === 'string' ? e.source : e.source.id) === tid)
        .map(e => (typeof e.target === 'string' ? e.target : e.target.id))
      
      childrenIds.forEach(cid => {
        canvasRef.value.updateNodeExecution(cid, { 
          waiting: true, 
          status: 'unknown' 
        })
      })
    }
  }

  try {
    const r = await post(`/workflows/${wfId.value}/run`)
    if (r.ok) {
      console.log('Workflow execution started in background:', r.run_id)
      // Now that we have the run_id, start polling specifically for this run.
      // Polling is intentionally started AFTER the POST so we never accidentally
      // match a previous completed run before our background task inserts its record.
      startPolling(r.run_id)
      if (canvasRef.value) {
        // Use a non-blocking call so runActive can finish its initial feedback
        canvasRef.value.runLivePlayback(
          () => lastRunResult.value?.node_results || [],
          () => isExecuting.value,
          () => backendDone.value
        ).then(() => {
          // Cleanup: reset any nodes still stuck in running/waiting after the run ends.
          // Covers: skipped IF branches, nodes after a stop-on-error halt, and any
          // node whose result never arrived before backendDone was set.
          if (canvasRef.value) {
            nodes.value.forEach(n => {
              const exec = n.data?.execution
              if (exec && (exec.running || exec.waiting)) {
                canvasRef.value.updateNodeExecution(n.id, {
                  running: false,
                  waiting: false,
                  status: (exec.status === 'running' || exec.status === 'unknown') ? 'unknown' : exec.status,
                })
              }
            })
          }
          isExecuting.value = false
          stopPolling()
          toast('Workflow flow complete', true)
        })
      }
    } else {
      isExecuting.value = false
      stopPolling()
      toast(r.error || 'Workflow failed', false)
      updateNodeExecutionStates([{ error: 'Failed' }]) 
    }
  } catch (e) {
    isExecuting.value = false
    stopPolling()
    toast('Execution error', false)
  }
}

function getWorkflowPayload() {
  const triggerNode = nodes.value.find(n => 
    n.data?.node_type === 'trigger' || n.data?.node_type === 'circadian' || n.data?.node_type === 'stimulus' ||
    n.data?.type === 'trigger' || n.data?.type === 'circadian' || n.data?.type === 'stimulus'
  )
  const triggerConfig = triggerNode?.data?.config || {}
  const triggerType = triggerConfig.type || trigger.value.type || 'manual'

  return {
    id: wfId.value && wfId.value !== 'new' ? wfId.value : undefined,
    name: wfName.value,
    description: wfDesc.value,
    // Error workflow (A3): handler to run on failure; null clears it.
    error_workflow_id: wfErrorWorkflowId.value || null,
    trigger_type: triggerType,
    trigger_config: triggerConfig,
    nodes: nodes.value.map((n) => ({
      id: n.id,
      node_type: n.data?.node_type || 'default',
      name: n.data?.label || n.data?.name || n.id,
      config: n.data?.config || {},
      enabled: n.data?.enabled !== false,
      continue_on_fail: n.data?.continueOnFail === true,
      retries: Number(n.data?.retries) || 0,
      retry_wait_ms: Number(n.data?.retryWaitMs) || 0,
      retry_backoff: n.data?.retryBackoff === 'exponential' ? 'exponential' : 'fixed',
      // Pinned output (A4): round-tripped so a normal save keeps the pin.
      pinned_data: n.data?.pinnedData ?? null,
      position: 0,
      position_x: n.position?.x || 0,
      position_y: n.position?.y || 0,
    })),
    edges: edges.value.map((e) => ({
      id: e.id,
      source_id: typeof e.source === 'string' ? e.source : e.source.id,
      target_id: typeof e.target === 'string' ? e.target : e.target.id,
      source_handle: e.sourceHandle,
      target_handle: e.targetHandle,
    })),
    enabled: true,
  }
}

/**
 * Logical sorting for nodes to ensure JSON follows execution flow
 */
function getLogicalNodeOrder(targetNodes, allEdges) {
  const nodeIds = new Set(targetNodes.map(n => n.id))
  const relevantEdges = allEdges.filter(e => nodeIds.has(e.source) && nodeIds.has(e.target))
  
  const ordered = []
  const visited = new Set()
  const queue = []

  // 1. Identify root nodes (nodes with no incoming edges from the targeted set, or trigger types)
  const setWithIncoming = new Set(relevantEdges.map(e => e.target))
  const roots = targetNodes.filter(n => !setWithIncoming.has(n.id) || n.data.type === 'trigger')
  
  // Sort roots: Triggers first, then alphabetically by label to keep it deterministic
  roots.sort((a, b) => {
    const aIsTrigger = a.data.type === 'trigger'
    const bIsTrigger = b.data.type === 'trigger'
    if (aIsTrigger && !bIsTrigger) return -1
    if (!aIsTrigger && bIsTrigger) return 1
    return (a.data.label || '').localeCompare(b.data.label || '')
  })

  roots.forEach(root => {
    if (!visited.has(root.id)) {
      queue.push(root)
      visited.add(root.id)
    }
  })

  // 2. Perform BFS traversal following the flow
  while (queue.length > 0) {
    const current = queue.shift()
    ordered.push(current)

    // Find children in the set
    const childrenIds = relevantEdges
      .filter(e => e.source === current.id)
      .map(e => e.target)
    
    childrenIds.forEach(id => {
      if (!visited.has(id)) {
        const childNode = targetNodes.find(n => n.id === id)
        if (childNode) {
          visited.add(id)
          queue.push(childNode)
        }
      }
    })
  }

  // 3. Cleanup: append any unconnected nodes that weren't reached via BFS
  targetNodes.forEach(n => {
    if (!visited.has(n.id)) {
      ordered.push(n)
      visited.add(n.id)
    }
  })

  return ordered
}

function isImageUrl(url) {
  if (!url) return false
  return (
    url.startsWith('http') ||
    url.startsWith('data:image') ||
    url.startsWith('/') ||
    url.includes('.svg') ||
    url.includes('.png') ||
    url.includes('.jpg')
  )
}

async function copyWorkflow(nodesToCopy = null) {
  if (!canvasRef.value) return

  const selectedNodes = nodesToCopy || canvasRef.value.getSelectedNodes()
  if (!selectedNodes || selectedNodes.length === 0) {
    console.log('[Copy] No nodes selected, skipping.')
    return
  }

  // APPLY LOGICAL SORTING
  const orderedNodes = getLogicalNodeOrder(selectedNodes, edges.value)
  console.log(`[Copy] Serializing ${orderedNodes.length} nodes in logical order...`)

  const selectedIds = orderedNodes.map(n => n.id)
  
  // Strict filter: only copy targeted nodes and edges that connect two targeted nodes
  const payload = {
    nodes: orderedNodes.map(n => ({
        id: n.id,
        node_type: n.data.node_type || n.type,
        name: n.data.label || n.data.name || n.id,
        config: JSON.parse(JSON.stringify(n.data.config || {})),
        enabled: n.data.enabled !== false,
        position_x: n.position?.x || 0,
        position_y: n.position?.y || 0,
      })),
    edges: edges.value
      .filter(e => {
          const s = typeof e.source === 'string' ? e.source : e.source.id;
          const t = typeof e.target === 'string' ? e.target : e.target.id;
          return selectedIds.includes(s) && selectedIds.includes(t);
      })
      .sort((a, b) => {
        // Sort edges by source node index in the ordered list
        const sA = typeof a.source === 'string' ? a.source : a.source.id;
        const sB = typeof b.source === 'string' ? b.source : b.source.id;
        return selectedIds.indexOf(sA) - selectedIds.indexOf(sB)
      })
      .map(e => ({
        id: e.id,
        source: typeof e.source === 'string' ? e.source : e.source.id,
        target: typeof e.target === 'string' ? e.target : e.target.id,
        source_handle: e.sourceHandle,
        target_handle: e.targetHandle,
      }))
  }

  const json = JSON.stringify(payload, null, 2)
  
  try {
    await navigator.clipboard.writeText(json)
    toast(`Copied ${selectedNodes.length} node${selectedNodes.length > 1 ? 's' : ''}`)
    
    // Optional: Clear selection after copy to show it was "captured" (matches some editor patterns)
    // if (canvasRef.value.removeSelectedNodes) canvasRef.value.removeSelectedNodes(selectedNodes)
  } catch (err) {
    console.error('[Copy] Clipboard failure:', err)
    toast('Copy failed. Check browser permissions.', false)
  }
}

async function pasteWorkflow(pastedData = null) {
  try {
    let data = pastedData
    if (!data) {
      // Use fallback if called manually without data
      try {
        const text = await navigator.clipboard.readText()
        data = JSON.parse(text)
      } catch (err) {
        toast('Please use Ctrl+V to paste', false)
        return
      }
    }
    
    if (!data.nodes || !Array.isArray(data.nodes)) {
      // Maybe it was raw JSON but not Axon format? Try to ignore quietly if it doesn't look like JSON at all
      return
    }

    console.log('[Paste] Importing nodes:', data.nodes.length)

    const idMap = {};

    // Original positions of the copied nodes.
    const srcPositions = data.nodes.map(n => ({
      x: (n.position_x !== undefined ? n.position_x : (n.position?.x || 0)),
      y: (n.position_y !== undefined ? n.position_y : (n.position?.y || 0)),
    }));

    // Decide where to drop the group. Prefer the last mouse position in the
    // canvas so the paste lands under the cursor; otherwise nudge by a small
    // offset so it doesn't sit exactly on top of the originals.
    const target = canvasRef.value?.getLastFlowPosition?.();
    let dx, dy;
    if (target) {
      const xs = srcPositions.map(p => p.x);
      const ys = srcPositions.map(p => p.y);
      // Center the pasted group's bounding box on the cursor.
      const cx = (Math.min(...xs) + Math.max(...xs)) / 2;
      const cy = (Math.min(...ys) + Math.max(...ys)) / 2;
      dx = target.x - cx;
      dy = target.y - cy;
    } else {
      dx = 60;
      dy = 60;
    }

    // Pasted nodes need fresh UNIQUE names, not just fresh ids: $node["Name"]
    // resolves by name (and the executor even seeds prior-run results), so a
    // pasted "Axon" that kept its name would silently route expressions to the
    // ORIGINAL "Axon". Re-base each pasted name (strip any trailing " N") and
    // re-number from the bare name — first copy of a free name keeps it, the
    // rest become "Name 1", "Name 2", ... Track names handed out within this
    // same paste so a multi-node paste never assigns the same name twice.
    const takenLabels = new Set(
      nodes.value.map(n => (n.data?.label || '').toLowerCase())
    )
    const uniquePasteName = (rawName, nodeType) => {
      const base = (rawName || '').replace(/\s+\d+$/, '').trim()
        || (NODE_TYPES[nodeType]?.displayName || 'Neuron')
      let name = base
      if (takenLabels.has(name.toLowerCase())) {
        let counter = 1
        while (takenLabels.has(`${base} ${counter}`.toLowerCase())) counter++
        name = `${base} ${counter}`
      }
      takenLabels.add(name.toLowerCase())
      return name
    }

    // Renames collected so $node["old"] references can be repointed to the
    // renamed copies below — and only those; refs to nodes outside the paste
    // are left untouched.
    const renames = [];

    const newNodes = data.nodes.map((n, i) => {
      const oldId = n.id;
      const newId = `node_${Date.now()}_${Math.random().toString(36).substr(2, 5)}`;
      idMap[oldId] = newId;

      const newName = uniquePasteName(n.name, n.node_type);
      if (n.name && newName !== n.name) renames.push({ oldName: n.name, newName });

      return {
        id: newId,
        type: 'canvas-node',
        position: {
          x: srcPositions[i].x + dx,
          y: srcPositions[i].y + dy,
        },
        data: createNodeData(newId, n.node_type, newName, n.config, n.enabled !== false)
      };
    });

    // Repoint references among the pasted nodes to their renamed copies. Done
    // in two phases via unique placeholders so a cascade — e.g. "Axon"->"Axon 1"
    // while another pasted node is itself "Axon 1"->"Axon 2" — can't double-
    // rewrite the same reference.
    renames.forEach((r, i) => renameNodeInExpressions(newNodes, r.oldName, `__axonPasteTmp${i}__`));
    renames.forEach((r, i) => renameNodeInExpressions(newNodes, `__axonPasteTmp${i}__`, r.newName));

    const newEdges = (data.edges || []).map(e => {
      const newId = `edge_${Date.now()}_${Math.random().toString(36).substr(2, 5)}`;
      const sourceId = idMap[e.source_id] || idMap[e.source] || e.source_id || e.source;
      const targetId = idMap[e.target_id] || idMap[e.target] || e.target_id || e.target;

      return {
        id: newId,
        source: sourceId,
        target: targetId,
        sourceHandle: e.source_handle || e.sourceHandle,
        targetHandle: e.target_handle || e.targetHandle,
        type: 'canvas-edge',
        data: createEdgeData(newId, sourceId, targetId, e.source_handle || e.sourceHandle, e.target_handle || e.targetHandle)
      };
    }).filter(e => e.source && e.target);

    // Merge into existing nodes/edges
    nodes.value = [...nodes.value, ...newNodes];
    edges.value = [...edges.value, ...newEdges];
    
    toast(`Pasted ${newNodes.length} nodes successfully`, true)
    save()

    // Keep the freshly pasted nodes selected (and deselect everything else) so
    // the user can immediately drag the whole group. Wait for Vue Flow to
    // register the new nodes before touching its selection state.
    if (newNodes.length > 0) {
      await nextTick()
      canvasRef.value?.selectNodes?.(newNodes.map(n => n.id))
    }
  } catch (e) {
    console.error('Paste error:', e)
    // Don't toast for random non-json pastes to avoid annoyance
    if (e instanceof SyntaxError) return;
    toast('Failed to paste: Invalid Axon format', false)
  }
}

async function save(opts = {}) {
  // Silent saves (autosave from NodeDetails / field blur) skip the success toast
  // so editing doesn't spam notifications; errors always surface.
  const silent = opts?.silent === true

  if (!wfName.value.trim()) {
    if (!silent) toast('Workflow name is required', false)
    return
  }

  const payload = getWorkflowPayload()

  try {
    const r = await post('/workflows', payload)
    if (r.ok) {
      if (!silent) toast('Workflow saved', true)
      if (!wfId.value && r.id) {
        wfId.value = r.id
      }
      load()
    } else {
      toast(r.error, false)
    }
  } catch (e) {
    toast('Failed to save workflow', false)
  }
}

async function removeWorkflow() {
  if (!wfId.value || !confirm(`Delete workflow "${wfName.value}"?`)) return
  try {
    const r = await del(`/workflows/${wfId.value}`)
    toast(r.ok ? 'Workflow deleted' : r.error, r.ok)
    selectedWorkflow.value = null
    load()
  } catch (e) {
    toast('Failed to delete workflow', false)
  }
}


function startPolling(runId) {
  if (pollTimer.value) clearInterval(pollTimer.value)
  isExecuting.value = true
  activeRunId.value = runId

  let isPolling = false // In-flight guard
  let pollCount = 0

  console.log(`[Poll] Starting polling for runId=${runId}`)

  pollTimer.value = setInterval(async () => {
    if (isPolling) return // Skip if previous request hasn't returned yet
    isPolling = true
    pollCount++

    try {
      // Use the lightweight single-run endpoint (direct PK lookup) instead
      // of GET /workflows/{id}/runs which fetches 10 runs with full JSON.
      const targetRun = await get(`/workflow-runs/${runId}`)

      if (pollCount <= 5 || pollCount % 10 === 0) {
        const nResults = Array.isArray(targetRun?.node_results) ? targetRun.node_results.length : 0
        console.log(`[Poll] #${pollCount} status=${targetRun?.status}, node_results=${nResults}`)
      }

      if (targetRun?.error) {
        // Run not found yet — wait for next tick
        if (pollCount <= 10 || pollCount % 20 === 0) {
          console.warn(`[Poll] #${pollCount} Run not found yet: ${targetRun.error}`)
        }
        isPolling = false
        return
      }

      lastRunResult.value = targetRun

      if (targetRun.status !== 'running') {
        const nResults = Array.isArray(targetRun.node_results) ? targetRun.node_results.length : 0
        console.log(`[Poll] Backend done! status=${targetRun.status}, final node_results=${nResults}`)
        clearInterval(pollTimer.value)
        pollTimer.value = null
        backendDone.value = true // Signal: all results are now in lastRunResult
      }
    } catch (e) {
      console.error('[Poll] Polling error', e)
    } finally {
      isPolling = false
    }
  }, 1500)
}

function stopPolling() {
  if (pollTimer.value) {
    clearInterval(pollTimer.value)
    pollTimer.value = null
  }
  isExecuting.value = false
}

function loadHistoryToEditor(run) {
  lastRunResult.value = { ...run, node_results: [...(run.node_results || [])] }
  updateNodeExecutionStates(lastRunResult.value.node_results)
  toast('Run data pinned to editor', true)
  // Optionally close history: showHistory.value = false
}

function updateNodeExecutionStates(nodeResults) {
  if (!nodeResults || !canvasRef.value) return
  console.log('UPDATING NODE STATES:', nodeResults)
  
  const executionMap = {}
  nodeResults.forEach(result => {
    const hasError = !!result.error
    executionMap[String(result.node_id)] = {
      running: false,
      waiting: false,
      status: hasError ? 'error' : 'success',
    }
  })
  
  canvasRef.value.updateAllNodesExecution(executionMap)
}


function edgeSource(edge) {
  return typeof edge.source === 'string' ? edge.source : edge.source.id
}

function edgeTarget(edge) {
  return typeof edge.target === 'string' ? edge.target : edge.target.id
}

async function loadHistory() {
  if (!wfId.value) return
  try {
    const d = await get(`/workflows/${wfId.value}/runs`)
    // Normalize: backend may return a plain array or { runs: [] }
    const runs = Array.isArray(d) ? d : (d.runs || [])
    expandedRuns.value[wfId.value] = runs
    
    // Default all runs to collapsed
    collapsedRuns.value = {}
    runs.forEach((_, i) => {
      collapsedRuns.value[i] = true
    })
    
    showHistory.value = true
  } catch (e) {
    toast('Failed to load history', false)
  }
}

function nodeOutput(nr) {
  const o = nr.output || {}
  const stdout = String(o.stdout || '').trim()
  const text = String(o.text_content || '').trim()
  const body = o.body
  if (stdout) return stdout
  if (text) return text
  if (body !== undefined && body !== null && body !== "")
    return typeof body === 'object'
      ? JSON.stringify(body, null, 2)
      : String(body).trim()
  if (
    nr.status === 'success' &&
    o &&
    typeof o === 'object' &&
    Object.keys(o).length > 0
  )
    return JSON.stringify(o, null, 2)
  return nr.status === 'success' ? '(No output)' : nr.status || 'completed'
}

function getUpstreamNodes(nodeId) {
  if (!nodeId) return []
  const upstreamIds = new Set()
  const queue = [nodeId]
  while (queue.length > 0) {
    const currentId = queue.shift()
    const parentEdges = edges.value.filter((e) => edgeTarget(e) === currentId)
    for (const edge of parentEdges) {
      const sourceId = edgeSource(edge)
      if (!upstreamIds.has(sourceId)) {
        upstreamIds.add(sourceId)
        queue.push(sourceId)
      }
    }
  }
  const orderedIds = Array.from(upstreamIds).reverse()
  return orderedIds.map(id => nodes.value.find(n => n.id === id)).filter(Boolean)
}

function getDownstreamNodes(nodeId) {
  if (!nodeId) return []
  const downstreamIds = new Set()
  const queue = [nodeId]
  while (queue.length > 0) {
    const currentId = queue.shift()
    const childEdges = edges.value.filter((e) => edgeSource(e) === currentId)
    for (const edge of childEdges) {
      const targetId = edgeTarget(edge)
      if (!downstreamIds.has(targetId)) {
        downstreamIds.add(targetId)
        queue.push(targetId)
      }
    }
  }
  const orderedIds = Array.from(downstreamIds)
  return orderedIds.map(id => nodes.value.find(n => n.id === id)).filter(Boolean)
}

function switchNode(nodeId) {
  const node = nodes.value.find(n => n.id === nodeId)
  if (node) {
    selectedNode.value = node
    isNodeDetailsOpen.value = true
  }
}

function removeNode(nodeId) {
  if (!nodeId) return

  // Auto-heal connections
  const incomingEdges = edges.value.filter(e => edgeTarget(e) === nodeId)
  const outgoingEdges = edges.value.filter(e => edgeSource(e) === nodeId)
  const healedEdges = []

  if (incomingEdges.length === 1 && outgoingEdges.length > 0) {
    const incomingEdge = incomingEdges[0]
    for (const outEdge of outgoingEdges) {
      const edgeId = `edge_${Date.now()}_${Math.random().toString(36).substr(2, 5)}`
      const sourceId = edgeSource(incomingEdge)
      const targetId = edgeTarget(outEdge)
      healedEdges.push({
        id: edgeId,
        source: sourceId,
        target: targetId,
        sourceHandle: incomingEdge.sourceHandle,
        targetHandle: outEdge.targetHandle,
        type: 'canvas-edge',
        data: createEdgeData(edgeId, sourceId, targetId, incomingEdge.sourceHandle, outEdge.targetHandle)
      })
    }
  }

  nodes.value = nodes.value.filter((n) => n.id !== nodeId)
  edges.value = edges.value.filter((e) => edgeSource(e) !== nodeId && edgeTarget(e) !== nodeId)
  
  if (healedEdges.length > 0) {
    edges.value.push(...healedEdges)
  }

  closeNodeDetails()
  // Auto-save if it's an existing workflow
  if (wfId.value && wfId.value !== 'new') {
    save()
  }
}

function handleConnect(newEdge) {
  // Prevent duplicate edges between the same source/target + handle pair
  const isDuplicate = edges.value.some(e => {
    const eSource = typeof e.source === 'string' ? e.source : e.source.id
    const eTarget = typeof e.target === 'string' ? e.target : e.target.id
    const nSource = typeof newEdge.source === 'string' ? newEdge.source : newEdge.source.id
    const nTarget = typeof newEdge.target === 'string' ? newEdge.target : newEdge.target.id
    return eSource === nSource && eTarget === nTarget &&
           e.sourceHandle === newEdge.sourceHandle && e.targetHandle === newEdge.targetHandle
  })
  if (isDuplicate) return

  // Prevent self-connections
  const src = typeof newEdge.source === 'string' ? newEdge.source : newEdge.source.id
  const tgt = typeof newEdge.target === 'string' ? newEdge.target : newEdge.target.id
  if (src === tgt) return

  edges.value.push(newEdge)
  if (wfId.value && wfId.value !== 'new') {
    save()
  }
}

function handleDisconnect(connection) {
  edges.value = edges.value.filter(
    (e) => !(
      edgeSource(e) === connection.source && 
      edgeTarget(e) === connection.target &&
      e.sourceHandle === connection.sourceHandle &&
      e.targetHandle === connection.targetHandle
    )
  )
  if (wfId.value && wfId.value !== 'new') {
    save()
  }
}

function handleUpdateNodes(updates) {
  // Update node positions after drag
  for (const update of updates) {
    const node = nodes.value.find(n => n.id === update.id)
    if (node) {
      node.position = update.position
    }
  }
  // Auto-save positions
  if (wfId.value && wfId.value !== 'new') {
    save()
  }
}

function handleTidyUp() {
  // Simple auto-layout - arrange nodes in a grid
  const gridSize = 200
  const cols = Math.ceil(Math.sqrt(nodes.value.length))

  nodes.value = nodes.value.map((n, i) => {
    const col = i % cols
    const row = Math.floor(i / cols)
    return {
      ...n,
      position: {
        x: col * gridSize + 100,
        y: row * gridSize + 100,
      },
    }
  })

  // Trigger fit view after layout
  nextTick(() => {
    if (canvasRef.value) {
      canvasRef.value.fitView()
    }
  })

  save()
}

function handleNodeContextMenu({ event, node }) {
  console.log('[Flow] Node context menu requested:', node.id, event.clientX, event.clientY)
  contextMenuVisible.value = true
  contextMenuPos.value = { x: event.clientX, y: event.clientY }
  contextMenuNode.value = node

  // Clamp the menu inside the viewport once it has rendered, so it's never
  // clipped off the right/bottom edge of the screen (flips back toward the click).
  nextTick(() => {
    const el = contextMenuRef.value
    if (!el) return
    const margin = 8
    const w = el.offsetWidth
    const h = el.offsetHeight
    let x = event.clientX
    let y = event.clientY
    if (x + w + margin > window.innerWidth) x = Math.max(margin, window.innerWidth - w - margin)
    if (y + h + margin > window.innerHeight) y = Math.max(margin, window.innerHeight - h - margin)
    contextMenuPos.value = { x, y }
  })
}

function closeContextMenu() {
  console.log('[Flow] Closing context menu')
  contextMenuVisible.value = false
  contextMenuNode.value = null
}

function handleContextCopy() {
  if (contextMenuNode.value) {
    // If multiple nodes are selected, copy selection. Otherwise copy this specific node.
    const selected = canvasRef.value?.getSelectedNodes()
    if (selected && selected.length > 1 && selected.some(n => n.id === contextMenuNode.value.id)) {
      copyWorkflow()
    } else {
      // Temporarily select this node to reuse copy logic
      const originalSelection = canvasRef.value?.getSelectedNodes()
      // Note: Vue Flow doesn't have a simple "select only" method easily callable here 
      // without affecting state, but we can just copy this one node.
      copyWorkflow([contextMenuNode.value])
    }
  }
  closeContextMenu()
}

async function handleContextRename() {
  if (contextMenuNode.value) {
    renamingNodeId.value = contextMenuNode.value.id
  }
  closeContextMenu()
}

// Rename initiated from the NodeDetails modal. NodeDetails already edits the
// page node in place (same array ref) and syncs $node["..."] expressions itself,
// so this only mirrors the final label into Vue Flow's internal store — without
// it the canvas keeps the old name until a reload. Mirrors handleNodeRename.
function handleDetailsRename({ id, name }) {
  const node = nodes.value.find(n => n.id === id)
  if (!node) return
  canvasRef.value?.updateNodeData(id, { label: name, name, labelEdited: node.data.labelEdited })
}

function handleNodeRename({ id, name }) {
  const node = nodes.value.find(n => n.id === id)
  if (node) {
    const oldLabel = node.data.label
    // Enforce uniqueness — duplicate labels break $node["Name"] resolution
    // (the resolver returns the first match). Mirrors the NodeDetails modal.
    const finalLabel = makeUniqueLabel((name || '').trim() || node.data.label, id)

    // A non-empty rename is an explicit user choice; flag it so the auto-label
    // logic in NodeDetails won't overwrite it when the action field changes.
    node.data.labelEdited = !!(name || '').trim()
    node.data.label = finalLabel
    node.data.name = finalLabel
    // Mirror into Vue Flow's store so the new label renders immediately. After a
    // run, execution updates sever the shared data reference, so mutating the
    // page array alone wouldn't reach the canvas until a reload.
    canvasRef.value?.updateNodeData(id, { labelEdited: node.data.labelEdited, label: finalLabel, name: finalLabel })
    // Keep active node details editor in sync
    if (selectedNode.value && selectedNode.value.id === id) {
      selectedNode.value.data.name = finalLabel
      selectedNode.value.data.label = finalLabel
    }

    // Sync {{ $node["OldName"]... }} references in other nodes so they don't
    // permanently miss after a rename. The NodeDetails modal already does this;
    // the right-click rename path previously didn't, leaving stale references.
    if (oldLabel && oldLabel !== finalLabel) {
      renameNodeInExpressions(nodes.value, oldLabel, finalLabel)
    }
    save()
  }
  renamingNodeId.value = null
}

function handleContextDelete() {
  if (contextMenuNode.value) {
    removeNode(contextMenuNode.value.id)
  }
  closeContextMenu()
}

function handleContextExecute() {
  if (contextMenuNode.value) {
    handleRunNode(contextMenuNode.value.id)
  }
  closeContextMenu()
}

function handleContextSettings() {
  if (contextMenuNode.value) {
    handleNodeActivate(contextMenuNode.value)
  }
  closeContextMenu()
}

function handleContextReplace() {
  if (contextMenuNode.value) {
    // Stash the node to swap, then open the picker. The actual replacement
    // runs in addNodeFromPalette once the user chooses a new neuron type.
    pendingReplace.value = contextMenuNode.value
    isNodePickerOpen.value = true
  }
  closeContextMenu()
}

// Swap a node's type in place. Keeping the same id preserves every edge that
// references it, so the node's connections survive the replacement. The config
// is reset to the new type's defaults (configs aren't portable between types).
function replaceNode(oldNodeId, newType) {
  const idx = nodes.value.findIndex(n => n.id === oldNodeId)
  if (idx === -1) return
  const oldNode = nodes.value[idx]

  const baseName = newType === 'trigger'
    ? 'When clicked'
    : (NODE_TYPES[newType]?.displayName || 'Neuron')
  const displayName = makeUniqueLabel(baseName, oldNodeId)

  const newData = createNodeData(oldNodeId, newType, displayName, getInitialConfig(newType))
  nodes.value[idx] = {
    ...oldNode,
    type: 'canvas-node',
    data: newData,
  }

  // Mirror into Vue Flow's store so the swapped node renders without a reload.
  canvasRef.value?.updateNodeData(oldNodeId, newData, { replace: true })

  // Keep the editor in sync if the replaced node was open/selected.
  if (selectedNode.value && selectedNode.value.id === oldNodeId) {
    selectedNode.value = nodes.value[idx]
  }

  save()
}

function handleKeydown(e) {
  if (['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName)) return

  // Prevent browser default Ctrl+S
  if ((e.ctrlKey || e.metaKey) && e.key === 's') {
    e.preventDefault()
    save()
    return
  }

  // Selective Copy
  if ((e.ctrlKey || e.metaKey) && e.key === 'c') {
    // If the user has highlighted text anywhere, let the browser copy it natively
    const selection = window.getSelection()
    if (selection && selection.toString().trim().length > 0) {
      return
    }

    // Check if we have nodes selected before preventing default
    const selectedNodes = canvasRef.value?.getSelectedNodes()
    if (selectedNodes && selectedNodes.length > 0) {
      e.preventDefault()
      copyWorkflow()
    }
  }
}

function handlePaste(e) {
  // Only trigger if not in an input/textarea
  if (['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName)) return
  if (document.activeElement.isContentEditable) return

  const text = e.clipboardData.getData('text')
  if (!text) return

  try {
    const data = JSON.parse(text)
    if (data.nodes || data.edges) {
      e.preventDefault()
      pasteWorkflow(data)
    }
  } catch (err) {
    // Not valid JSON, let regular paste happen or ignore
  }
}

function handleClickOutside(e) {
  // If right-click, don't let this close the menu immediately as we're likely opening a new one
  if (e.button === 2) return

  // If clicking inside the history panel, don't close it
  if (showHistory.value && historyRef.value && historyRef.value.contains(e.target)) {
    return
  }

  // Handle History closure
  if (showHistory.value && historyRef.value && !historyRef.value.contains(e.target)) {
    // Check if the click was on the "History" button itself (to avoid toggle conflict)
    const historyBtn = document.querySelector('.btn-warn') // History button is btn-warn
    if (historyBtn && historyBtn.contains(e.target)) return

    showHistory.value = false
  }

  // If clicking inside the context menu itself, don't close it (let the item click handler win)
  if (contextMenuVisible.value && contextMenuRef.value && contextMenuRef.value.contains(e.target)) {
    return
  }

  // Handle Node Picker closure
  if (isNodePickerOpen.value && nodePickerRef.value) {
    const isClickInside = nodePickerRef.value.contains(e.target)
    if (!isClickInside) {
      isNodePickerOpen.value = false
      pendingReplace.value = null
    }
  }

  if (isWorkflowMenuOpen.value && workflowMenuRef.value && !workflowMenuRef.value.contains(e.target)) {
    isWorkflowMenuOpen.value = false
  }

  // Handle Context Menu closure
  if (contextMenuVisible.value) {
    closeContextMenu()
  }
}

async function loadFonts() {
  try {
    const d = await get('/fonts')
    const fonts = d.fonts || []
    const fontOptions = fonts.map(f => ({ name: f, value: f }))
    
    // Auto-select Playball if it exists, otherwise keep empty
    const defaultVal = fonts.find(f => f.toLowerCase().includes('playball')) || (fonts.length > 0 ? fonts[0] : '')
    
    if (NODE_TYPES.fovea) {
      const quoteGroup = NODE_TYPES.fovea.properties.find(p => p.name === 'quote_settings_group')
      if (quoteGroup) {
        const mainFontProp = quoteGroup.options.find(p => p.name === 'font_path')
        if (mainFontProp) {
          mainFontProp.options = fontOptions
          mainFontProp.default = defaultVal
        }
      }

      const attrGroup = NODE_TYPES.fovea.properties.find(p => p.name === 'attribution_settings_group')
      if (attrGroup) {
        const attrFontProp = attrGroup.options.find(p => p.name === 'attribution_font_path')
        if (attrFontProp) {
          attrFontProp.options = [{name: 'Default (Same as Main)', value: ''}, ...fontOptions]
          attrFontProp.default = ''
        }
      }
    }
  } catch (e) {
    console.warn('Failed to load fonts:', e)
  }
}

onMounted(() => {
  load()
  loadMcpTools()
  loadFonts()
  loadSpreadsheets()
  loadCalendars()
  window.addEventListener('mousedown', handleClickOutside)
  window.addEventListener('keydown', handleKeydown)
  window.addEventListener('paste', handlePaste)
})

onUnmounted(() => {
  window.removeEventListener('mousedown', handleClickOutside)
  window.removeEventListener('keydown', handleKeydown)
  window.removeEventListener('paste', handlePaste)
})
</script>

<template>
  <div class="workflow-explorer">
    <!-- Legacy sidebar kept disabled in favor of the dropdown selector -->
    <aside v-if="false" class="workflow-sidebar">
      <div class="sidebar-header">
        <div class="sidebar-header-copy">
          <span class="sidebar-kicker">Builder</span>
          <h2>Workflows</h2>
        </div>
        <div class="sidebar-header-actions">
          <button class="btn btn-xs btn-success" @click="createNew">New</button>
          <button
            v-if="!isWorkflowSidebarCollapsed"
            class="workflow-sidebar-toggle"
            type="button"
            @click="toggleWorkflowSidebar"
            :title="isWorkflowSidebarCollapsed ? 'Show workflows' : 'Hide workflows'"
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M6 5v14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
              <path
                :d="isWorkflowSidebarCollapsed ? 'M11 8l4 4-4 4' : 'M15 8l-4 4 4 4'"
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
      <div class="workflow-list">
        <button
          v-for="wf in workflows"
          :key="wf.id"
          class="workflow-item"
          :class="{ active: selectedWorkflow?.id === wf.id }"
          @click="selectWorkflow(wf)"
        >
          <div class="wf-dot" :class="wf.last_status"></div>
          <div class="wf-info">
            <div class="wf-name">{{ wf.name }}</div>
            <div class="wf-meta">
              {{ wf.trigger_type }} • {{ wf.nodes?.length || 0 }} neurons
            </div>
          </div>
        </button>
        <div v-if="workflows.length === 0" class="empty-sidebar">No workflows</div>
      </div>
    </aside>

    <!-- Main Content -->
    <main class="workflow-main">
      <template v-if="selectedWorkflow">
        <header class="workflow-toolbar">
          <div class="toolbar-left">
            <div ref="workflowMenuRef" class="workflow-menu">
              <button
                class="workflow-menu-trigger"
                type="button"
                @click.stop="toggleWorkflowMenu"
                :aria-expanded="isWorkflowMenuOpen ? 'true' : 'false'"
                title="Choose workflow"
              >
                <span class="workflow-menu-trigger-label">{{ selectedWorkflow?.name || 'Choose workflow' }}</span>
                <span class="workflow-menu-trigger-icon">{{ isWorkflowMenuOpen ? '-' : '+' }}</span>
              </button>

              <Transition name="scale-fade">
                <div v-if="isWorkflowMenuOpen" class="workflow-menu-panel">
                  <div class="workflow-menu-header">
                    <span class="workflow-menu-kicker">Workflows</span>
                  </div>
                  <input
                    v-model="workflowMenuSearch"
                    type="text"
                    class="workflow-menu-search"
                    placeholder="Search workflows..."
                  />
                  <div class="workflow-menu-list">
                    <button
                      v-for="wf in filteredWorkflows"
                      :key="wf.id"
                      class="workflow-menu-item"
                      :class="{ active: selectedWorkflow?.id === wf.id }"
                      type="button"
                      @click="selectWorkflow(wf)"
                    >
                      <div class="wf-dot" :class="wf.last_status"></div>
                      <div class="workflow-menu-copy">
                        <div class="workflow-menu-name">{{ wf.name }}</div>
                        <div class="workflow-menu-meta">{{ wf.trigger_type }} | {{ wf.nodes?.length || 0 }} nodes</div>
                      </div>
                    </button>
                    <div v-if="filteredWorkflows.length === 0" class="workflow-menu-empty">No workflows found</div>
                  </div>
                </div>
              </Transition>
            </div>
            <button class="btn btn-sm btn-success" type="button" @click="createNew">New</button>
            <input
              v-model="wfName"
              type="text"
              class="wf-title-input"
              placeholder="Workflow Name"
              @change="save({ silent: true })"
            />
          </div>
          <div class="toolbar-right">
            <div ref="wfSettingsRef" class="wf-settings-wrap" style="position:relative;">
              <button class="btn btn-sm workflow-action-btn btn-neutral" title="Workflow settings" @click.stop="showWfSettings = !showWfSettings">⚙</button>
              <Transition name="scale-fade">
                <div v-if="showWfSettings" class="wf-settings-pop" @click.stop>
                  <div class="wf-settings-title">Workflow Settings</div>
                  <label class="wf-settings-label">On failure, run workflow</label>
                  <select v-model="wfErrorWorkflowId" class="wf-settings-select" @change="save({ silent: true })">
                    <option :value="null">— Global default —</option>
                    <option v-for="w in errorHandlerOptions" :key="w.id" :value="w.id">{{ w.name }}</option>
                  </select>
                  <div class="wf-settings-hint">Runs the chosen workflow's Error trigger when this one errors. Build a handler by adding a Stimulus neuron with trigger type “On Error”.</div>
                  <div class="wf-settings-divider"></div>
                  <label class="wf-settings-label">Backup &amp; share</label>
                  <div class="wf-settings-actions">
                    <button class="btn btn-sm btn-neutral" @click="exportWorkflow">Export JSON</button>
                    <button class="btn btn-sm btn-neutral" @click="triggerImport">Import JSON</button>
                  </div>
                  <div class="wf-settings-hint">Export bundles nodes, edges &amp; pins (never secrets). Imported workflows arrive disabled — re-map credentials, then enable.</div>
                </div>
              </Transition>
              <input ref="importFileRef" type="file" accept="application/json,.json" style="display:none" @change="importWorkflow" />
            </div>
            <button v-if="isExecuting" class="btn btn-sm workflow-action-btn btn-danger" @click="stopWorkflow">Stop</button>
            <button class="btn btn-sm workflow-action-btn btn-danger" @click="removeWorkflow">Delete</button>
            <button class="btn btn-sm workflow-action-btn btn-neutral" @click.stop="loadHistory">History</button>
            <button class="btn btn-sm workflow-action-btn btn-primary" @click="runActive">Run</button>
            <button class="btn btn-sm workflow-action-btn btn-success" @click="save">Save</button>
          </div>
        </header>

        <div class="canvas-wrapper">
          <!-- Collapsible Node Picker (n8n-style) -->
          <div ref="nodePickerRef" class="node-picker-container" :class="{ open: isNodePickerOpen }">
            <Transition name="scale-fade">
              <div v-if="isNodePickerOpen" class="floating-palette">
                <div class="palette-label">Neurons</div>
                <div class="palette-search">
                  <input ref="nodeSearchInputRef" type="text" v-model="nodeSearchQuery" @blur="nodeSearchQuery = ''" placeholder="Search neurons..." class="palette-search-input" />
                </div>
                <div class="palette-list">
                  <div
                    v-for="nt in filteredNodeTypes"
                    :key="nt.name"
                    class="palette-btn"
                    draggable="true"
                    @dragstart="onPaletteDragStart($event, nt.name)"
                    @mousedown="addNodeFromPalette(nt.name)"
                  >
                    <span>
                      <img v-if="isImageUrl(nt.icon)" :src="nt.icon" class="palette-icon-img" />
                      <template v-else>{{ nt.icon }}</template>
                    </span> {{ nt.displayName }}
                  </div>
                  <div v-if="filteredNodeTypes.length === 0" class="palette-empty">No neurons found</div>
                </div>
              </div>
            </Transition>
            
            <button 
              v-if="!isNodePickerOpen"
              class="node-picker-trigger" 
              @click.stop="isNodePickerOpen = !isNodePickerOpen"
              title="Add Neuron"
            >
              <span class="plus-icon">＋</span>
            </button>
          </div>

          <!-- n8n-style Workflow Canvas -->
          <WorkflowCanvas
            class="workflow-canvas-surface"
            ref="canvasRef"
            :nodes="nodes"
            :edges="edges"
            :executing="isExecuting"
            :renaming-node-id="renamingNodeId"
            @node-select="handleNodeSelect"
            @node-deselect="handleNodeDeselect"
            @node-context-menu="handleNodeContextMenu"
            @connect="handleConnect"
            @disconnect="handleDisconnect"
            @update:nodes="handleUpdateNodes"
            @add-node="handleAddNode"
            @add-from-handle="handleHandleAdd"
            @splice-node="handleSpliceNode"
            @insert-node="handleInsertNode"
            @delete-node="removeNode"
            @rename="handleNodeRename"
            @run-node="handleRunNode"
            @toggle-node="handleToggleNodeEnabled"
            @activate-node="handleNodeActivate"
            @tidy-up="handleTidyUp"
            @viewport-change="closeContextMenu"
          />

          <!-- Node Context Menu — teleported to <body> so position:fixed is
               relative to the viewport. Inside the canvas it would inherit a
               transformed/backdrop-filtered ancestor as its containing block,
               which broke the off-screen clamp. -->
          <Teleport to="body">
            <div
              v-if="contextMenuVisible"
              ref="contextMenuRef"
              class="node-context-menu"
              :style="{ top: `${contextMenuPos.y}px`, left: `${contextMenuPos.x}px` }"
              @click.stop
            >
              <div class="context-item" @click="handleContextExecute">
                <span class="c-icon">▶</span> Run Node
              </div>
              <div class="context-divider"></div>
              <div class="context-item" @click="handleContextSettings">
                <span class="c-icon">⚙</span> Settings
              </div>
              <div class="context-item" @click="handleContextReplace">
                <span class="c-icon">⇄</span> Replace
              </div>
              <div class="context-item" @click="handleContextRename">
                <span class="c-icon">✏</span> Rename
              </div>
              <div class="context-divider"></div>
              <div class="context-item" @click="handleContextCopy">
                <span class="c-icon">❐</span> Copy
              </div>
              <div class="context-item delete" @click="handleContextDelete">
                <span class="c-icon">🗑</span> Delete
              </div>
            </div>
          </Teleport>


          <!-- Advanced Node Editor Modal -->
          <Transition name="fade">
            <NodeDetails
              v-if="isNodeDetailsOpen && selectedNode"
              :node="selectedNode"
              :nodes="nodes"
              :upstream-nodes="getUpstreamNodes(selectedNode.id)"
              :downstream-nodes="getDownstreamNodes(selectedNode.id)"
              :last-run="lastRunResult"
              :workflow-id="wfId"
              :executing="isExecuting"
              :mcp-dynamic-props="getMcpDynamicProps"
              @close="closeNodeDetails"
              @save="save"
              @rename="handleDetailsRename"
              @execute="executeNodeStep"
              @delete="removeNode"
              @switch="switchNode"
              @clear-execution="clearNodeExecution"
              @output-added="handleDynamicOutputAdded"
              @output-removed="handleDynamicOutputRemoved"
            />
          </Transition>

          <!-- Run History Overlay -->
          <Transition name="slide-rtl">
            <div v-if="showHistory" ref="historyRef" class="side-panel history-panel">
              <div class="panel-header">
                <h3>Execution History</h3>
                <button class="close-btn" @click="showHistory = false">✕</button>
              </div>
              <div class="panel-body">
                <div v-for="(r, i) in (expandedRuns[wfId] || [])" :key="i" class="history-item" :class="r.status">
                  <div class="history-item-header" @click="collapsedRuns[i] = !collapsedRuns[i]">
                    <span class="collapse-icon">{{ collapsedRuns[i] ? '▸' : '▾' }}</span>
                    <div class="h-meta">{{ timeAgo(r.started_at) }} • <span class="h-status">{{ r.status }}</span></div>
                    <button class="btn btn-icon btn-pin-history" title="Pin Data to Editor" @click.stop="loadHistoryToEditor(r)">
                      📌 Pin
                    </button>
                  </div>
                  
                  <Transition name="fade">
                    <div v-if="!collapsedRuns[i]" class="history-item-details">
                      <div v-for="(nr, ni) in (r.node_results || [])" :key="ni" class="h-node">
                        <div class="h-node-name">{{ nr.node_name }}</div>
                        <div class="h-node-out">{{ nodeOutput(nr) }}</div>
                      </div>
                    </div>
                  </Transition>
                </div>
                <div v-if="!expandedRuns[wfId] || expandedRuns[wfId].length === 0" class="empty">No runs yet</div>
              </div>
            </div>
          </Transition>
        </div>
      </template>

      <div v-else class="workflow-empty-state">
        <div class="empty-icon">📂</div>
        <h3>Select a workflow to start editing</h3>
        <p>Or create a new one using the button in the sidebar.</p>
        <button class="btn btn-primary" @click="createNew">Create New Workflow</button>
      </div>
    </main>
  </div>
</template>

<style scoped>
.workflow-explorer {
  display: flex;
  height: 100%;
  background: radial-gradient(circle at 50% 50%, #1a1b26 0%, #0f1117 100%);
  overflow: hidden;
}

/* Sidebar */
.workflow-sidebar {
  width: 240px;
  background: rgba(20, 21, 30, 0.4);
  backdrop-filter: blur(40px);
  border-right: 1px solid rgba(255, 255, 255, 0.05);
  display: flex;
  flex-direction: column;
  flex-shrink: 0;
  z-index: 25;
}

.sidebar-header {
  padding: 24px 20px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid rgba(255, 255, 255, 0.03);
}

.sidebar-header h2 {
  font-size: 11px;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.15em;
  color: rgba(255, 255, 255, 0.4);
  margin: 0;
}

.workflow-list {
  flex: 1;
  overflow-y: auto;
  padding: 12px;
}

.workflow-list::-webkit-scrollbar { width: 4px; }
.workflow-list::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.1); border-radius: 10px; }

.workflow-item {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 12px 14px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 12px;
  color: #fff;
  cursor: pointer;
  text-align: left;
  transition: all 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  margin-bottom: 6px;
}

.workflow-item:hover {
  background: rgba(255, 255, 255, 0.04);
  transform: translateX(4px);
}

.workflow-item.active {
  background: rgba(129, 230, 217, 0.08);
  border-color: rgba(129, 230, 217, 0.2);
  box-shadow: 0 4px 15px rgba(0, 0, 0, 0.2);
}

.wf-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #444;
  position: relative;
}
.wf-dot.success {
  background: var(--green, #50fa7b);
  box-shadow: 0 0 10px var(--green, #50fa7b);
}
.wf-dot.error {
  background: var(--red, #ff5555);
  box-shadow: 0 0 10px var(--red, #ff5555);
}

.wf-name {
  font-weight: 600;
  font-size: 13.5px;
  margin-bottom: 2px;
}

.wf-meta {
  font-size: 10px;
  color: rgba(255, 255, 255, 0.35);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

/* Main Area */
.workflow-main {
  flex: 1;
  display: flex;
  flex-direction: column;
  position: relative;
  overflow: hidden;
}

.workflow-toolbar {
  height: 64px;
  width: 100%;
  background: rgba(15, 17, 23, 0.6);
  backdrop-filter: blur(20px);
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0 24px;
  z-index: 20;
}

.toolbar-left {
  display: flex;
  align-items: center;
  gap: 16px;
}

.wf-title-input {
  background: transparent;
  border: none;
  font-size: 18px;
  font-weight: 700;
  color: #fff;
  padding: 4px 0;
  width: 350px;
  outline: none !important;
  transition: all 0.2s;
}

.wf-title-input:focus {
  color: var(--teal, #81e6d9);
}

.toolbar-right {
  display: flex;
  gap: 12px;
}

.canvas-wrapper {
  flex: 1;
  position: relative;
  overflow: hidden;
}

/* Node Picker (n8n-style) */
.node-picker-container {
  position: absolute;
  top: 24px;
  left: 24px;
  z-index: 100;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 12px;
}

.node-picker-trigger {
  width: 52px;
  height: 52px;
  border-radius: 50%;
  background: linear-gradient(135deg, rgba(129, 230, 217, 0.45) 0%, rgba(129, 230, 217, 0.25) 100%);
  backdrop-filter: blur(20px);
  border: 1px solid rgba(129, 230, 217, 0.4);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.4), inset 0 0 20px rgba(129, 230, 217, 0.2);
  transition: all 0.4s cubic-bezier(0.16, 1, 0.3, 1);
  -webkit-user-select: none;
  user-select: none;
}

.node-picker-trigger:hover {
  transform: scale(1.1) rotate(5deg);
  background: linear-gradient(135deg, rgba(129, 230, 217, 0.6) 0%, rgba(129, 230, 217, 0.4) 100%);
  box-shadow: 0 15px 40px rgba(129, 230, 217, 0.3);
}

.node-picker-trigger.is-active {
  background: #1a1b26;
  border-color: var(--color--primary, #81e6d9);
  box-shadow: 0 0 20px rgba(129, 230, 217, 0.4);
}

.plus-icon {
  font-size: 24px;
  font-weight: 300;
}

.floating-palette {
  width: 200px;
  background: rgba(20, 21, 28, 0.85);
  backdrop-filter: blur(40px) saturate(180%);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 20px;
  padding: 12px 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  box-shadow: 0 30px 60px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.05);
  transform-origin: top left;
}

/* Transitions */
.scale-fade-enter-active,
.scale-fade-leave-active {
  transition: all 0.3s cubic-bezier(0.16, 1, 0.3, 1);
}

.scale-fade-enter-from,
.scale-fade-leave-to {
  opacity: 0;
  transform: scale(0.6) translateY(-20px);
}

  .palette-label {
    font-size: 10px;
    font-weight: 800;
    text-transform: uppercase;
    color: rgba(255, 255, 255, 0.4);
    margin-bottom: 8px;
    padding: 0 10px;
    letter-spacing: 0.15em;
    flex-shrink: 0;
  }

  .palette-search {
    padding: 0 8px 8px 8px;
    flex-shrink: 0;
  }

  .palette-search-input {
    width: 100%;
    background: rgba(0, 0, 0, 0.2);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 8px;
    padding: 6px 10px;
    color: white;
    font-size: 12px;
    outline: none;
    transition: border-color 0.2s;
  }

  .palette-search-input:focus {
    border-color: rgba(255, 255, 255, 0.3);
  }

  .palette-list {
    flex-grow: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 0 4px;
    min-height: 100px;
    max-height: 350px;
  }

  /* Custom Scrollbar for palette list */
  .palette-list::-webkit-scrollbar {
    width: 4px;
  }
  .palette-list::-webkit-scrollbar-track {
    background: transparent;
  }
  .palette-list::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.1);
    border-radius: 4px;
  }
  .palette-list::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.2);
  }

  .palette-empty {
    padding: 16px;
    text-align: center;
    color: rgba(255, 255, 255, 0.4);
    font-size: 12px;
  }

.palette-btn {
  display: flex;
  align-items: center;
  gap: 14px;
  background: transparent;
  border: none;
  color: rgba(255, 255, 255, 0.75);
  font-size: 13px;
  font-weight: 600;
  padding: 10px 12px;
  border-radius: 10px;
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  text-align: left;
}

.palette-btn:hover {
  background: rgba(255, 255, 255, 0.05);
  color: #fff;
  transform: translateX(6px);
}

.palette-btn span {
  font-size: 20px;
  width: 24px;
  display: flex;
  justify-content: center;
  filter: drop-shadow(0 0 8px rgba(0,0,0,0.5));
}

.palette-icon-img {
  width: 20px;
  height: 20px;
  object-fit: contain;
}

/* Side Panels */
.side-panel {
  position: absolute;
  top: 12px;
  right: 12px;
  bottom: 12px;
  width: 420px;
  background: rgba(15, 17, 23, 0.8);
  backdrop-filter: blur(30px);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 20px;
  z-index: 100;
  display: flex;
  flex-direction: column;
  box-shadow: -20px 0 60px rgba(0, 0, 0, 0.6);
  overflow: hidden;
}

/* Execution History is compact — narrower than the node-details side panel */
.history-panel {
  width: 320px;
}
.history-panel .panel-header {
  padding: 16px 18px;
}
.history-panel .panel-body {
  padding: 14px 18px;
}

.panel-header {
  padding: 24px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
}

.panel-header h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 800;
  color: #fff;
}

.panel-body {
  flex: 1;
  overflow-y: auto;
  padding: 24px;
}

.close-btn {
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.05);
  border-radius: 50%;
  color: #888;
  cursor: pointer;
  transition: all 0.2s;
}

.close-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #fff;
}

/* History Items */
.history-item {
  background: rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 16px;
  margin-bottom: 12px;
  transition: all 0.2s;
}

.history-item:hover {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(255, 255, 255, 0.1);
}

.h-meta {
  font-size: 11px;
  font-weight: 700;
  color: rgba(255, 255, 255, 0.3);
  text-transform: uppercase;
  letter-spacing: 0.1em;
  margin-bottom: 12px;
}

.history-item.success .h-meta { color: var(--green, #50fa7b); }
.history-item.error .h-meta { color: var(--red, #ff5555); }

.h-node {
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px dashed rgba(255, 255, 255, 0.05);
}

.h-node-name {
  font-size: 12px;
  font-weight: 700;
  color: #aaa;
  margin-bottom: 4px;
}

.h-node-out {
  background: rgba(0,0,0,0.2);
  padding: 10px;
  border-radius: 8px;
  font-family: 'Fira Code', monospace;
  font-size: 11px;
  white-space: pre-wrap; /* text-wrap: wrap */
  word-break: break-all; /* word-break: break-all */
  overflow-wrap: break-word;
  color: rgba(255,255,255,0.7);
  margin-top: 4px;
}

.history-item-header {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  user-select: none;
  font-weight: 500;
}
.btn-pin-history {
  margin-left: auto;
  font-size: 0.8rem;
  padding: 2px 8px;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.1);
  border: none;
  color: #fff;
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.2s, background 0.2s;
}
.history-item-header:hover .btn-pin-history {
  opacity: 1;
}
.btn-pin-history:hover {
  background: rgba(255, 255, 255, 0.2);
}

.history-item-details {
  padding-left: 24px;
  border-left: 1px dashed rgba(255,255,255,0.05);
  margin-top: 5px;
  margin-bottom: 5px;
}

.collapse-icon {
  font-size: 14px;
  color: rgba(255,255,255,0.3);
  width: 14px;
  display: inline-block;
  text-align: center;
}

.h-status {
  text-transform: capitalize;
  font-weight: 700;
}

/* Transitions */
.slide-rtl-enter-active,
.slide-rtl-leave-active {
  transition: transform 0.4s cubic-bezier(0.16, 1, 0.3, 1), opacity 0.4s ease;
}
.slide-rtl-enter-from,
.slide-rtl-leave-to {
  transform: translateX(40px);
  opacity: 0;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.3s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.workflow-empty-state {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  padding: 40px;
}

/* Context Menu */
.node-context-menu {
  position: fixed;
  z-index: 9999;
  width: 180px;
  background: rgba(15, 17, 23, 0.9);
  backdrop-filter: blur(24px) saturate(160%);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  padding: 6px;
  box-shadow: 
    0 15px 50px rgba(0, 0, 0, 0.7), 
    0 0 0 1px rgba(255, 255, 255, 0.05),
    inset 0 0 20px rgba(255, 255, 255, 0.02);
  animation: context-menu-fade 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  transform-origin: top left;
}

@keyframes context-menu-fade {
  from { opacity: 0; transform: scale(0.9) translateY(-10px); }
  to { opacity: 1; transform: scale(1) translateY(0); }
}

.context-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 12px;
  border-radius: 8px;
  font-size: 13px;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.85);
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
}

.context-item:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #fff;
  transform: translateX(4px);
}

.context-item.delete:hover {
  background: rgba(244, 63, 94, 0.15);
  color: #fb7185;
}

.context-divider {
  height: 1px;
  background: rgba(255, 255, 255, 0.08);
  margin: 6px 8px;
}

.c-icon {
  width: 18px;
  display: flex;
  justify-content: center;
  font-size: 14px;
  color: var(--color--primary, #81e6d9);
  opacity: 0.7;
}

.context-item:hover .c-icon {
  opacity: 1;
  transform: scale(1.1);
}

.empty-icon {
  font-size: 80px;
  margin-bottom: 24px;
  filter: grayscale(1) opacity(0.2) drop-shadow(0 0 20px rgba(255,255,255,0.1));
}

.empty-state h3 { font-size: 20px; font-weight: 800; color: #fff; margin-bottom: 12px; }
.empty-state p { color: rgba(255, 255, 255, 0.4); max-width: 300px; margin-bottom: 24px; }

/* Buttons */
.btn {
  font-weight: 700;
  letter-spacing: 0.02em;
  border-radius: 10px;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.1);
}

.btn:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
}

.btn:active {
  transform: translateY(0);
}

.btn-primary {
  background: linear-gradient(135deg, #6c5ce7 0%, #a29bfe 100%);
  border: none;
  color: #fff;
}

.btn-success {
  background: linear-gradient(135deg, #00b894 0%, #55efc4 100%);
  border: none;
  color: #fff;
}

.btn-ghost {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #fff;
}

.btn-ghost:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.2);
}

/* Layout refresh */
.workflow-explorer {
  position: relative;
  height: 100%;
  min-height: 100%;
  background:
    linear-gradient(180deg, rgba(37, 194, 209, 0.06), transparent 16%),
    linear-gradient(135deg, #0a1217 0%, #0e171d 100%);
}

.workflow-sidebar {
  width: 320px;
  background: rgba(13, 20, 24, 0.92);
  border-right: 1px solid rgba(255, 255, 255, 0.06);
  backdrop-filter: blur(20px);
  -webkit-backdrop-filter: blur(20px);
  transition: width 0.28s ease, transform 0.28s ease, opacity 0.28s ease;
}

.workflow-explorer.sidebar-collapsed:not(.compact-layout) .workflow-sidebar {
  width: 0;
  opacity: 0;
  border-right: 0;
  pointer-events: none;
  overflow: hidden;
}

.workflow-sidebar-scrim {
  position: absolute;
  inset: 0;
  z-index: 24;
  background: rgba(5, 9, 12, 0.58);
  backdrop-filter: blur(4px);
  -webkit-backdrop-filter: blur(4px);
}

.sidebar-header {
  padding: 18px;
  gap: 12px;
}

.sidebar-header-copy {
  min-width: 0;
}

.sidebar-kicker {
  display: inline-block;
  margin-bottom: 8px;
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.14em;
  color: rgba(255, 255, 255, 0.4);
}

.sidebar-header h2 {
  font-size: 18px;
  font-weight: 700;
  letter-spacing: 0;
  color: #fff;
}

.sidebar-header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.workflow-sidebar-toggle {
  width: 32px;
  height: 32px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 0;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 999px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.08), rgba(255, 255, 255, 0.03)),
    rgba(255, 255, 255, 0.02);
  color: rgba(255, 255, 255, 0.86);
  cursor: pointer;
  transition: transform 0.18s ease, background 0.18s ease, border-color 0.18s ease;
  box-shadow:
    inset 0 1px 0 rgba(255, 255, 255, 0.05),
    0 10px 20px rgba(0, 0, 0, 0.16);
}

.workflow-sidebar-toggle:hover {
  transform: translateY(-1px);
  border-color: rgba(37, 194, 209, 0.24);
  background:
    linear-gradient(180deg, rgba(37, 194, 209, 0.18), rgba(255, 255, 255, 0.04)),
    rgba(255, 255, 255, 0.03);
}

.workflow-sidebar-toggle svg {
  width: 14px;
  height: 14px;
}

.workflow-sidebar-toggle-toolbar {
  flex-shrink: 0;
}

.workflow-list {
  padding: 12px;
}

.workflow-item {
  margin-bottom: 8px;
  padding: 14px;
  border-radius: 8px;
  border: 1px solid transparent;
  background: rgba(255, 255, 255, 0.025);
}

.workflow-item:hover {
  transform: none;
  border-color: rgba(255, 255, 255, 0.06);
}

.workflow-item.active {
  background: linear-gradient(135deg, rgba(37, 194, 209, 0.14), rgba(143, 140, 255, 0.08));
  border-color: rgba(37, 194, 209, 0.22);
}

.workflow-main {
  min-width: 0;
  min-height: 0;
}

.workflow-toolbar {
  height: auto;
  min-height: 44px;
  padding: 6px 10px;
  gap: 8px;
  background: rgba(10, 16, 20, 0.7);
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.toolbar-left {
  flex: 1 1 auto;
  min-width: 0;
  gap: 8px;
}

.toolbar-right {
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}

.wf-title-input {
  width: min(100%, 520px);
  font-size: 15px;
  font-weight: 700;
  padding: 2px 0;
}

.wf-title-input::placeholder {
  color: rgba(255, 255, 255, 0.3);
}

.canvas-wrapper {
  flex: 1;
  min-height: 0;
}

.workflow-canvas-surface {
  width: 100%;
  height: 100%;
  display: block;
}

.node-picker-container {
  top: 16px;
  left: 16px;
  z-index: 30;
}

.node-picker-trigger {
  width: 40px;
  height: 40px;
  border-radius: 999px;
  background:
    linear-gradient(135deg, rgba(37, 194, 209, 0.92), rgba(75, 198, 147, 0.88));
  border: 1px solid rgba(255, 255, 255, 0.14);
  color: #062028;
  box-shadow: 0 18px 36px rgba(0, 0, 0, 0.26);
}

.node-picker-trigger:hover {
  transform: translateY(-1px) scale(1.03);
  box-shadow: 0 22px 42px rgba(0, 0, 0, 0.3);
}

.plus-icon {
  font-size: 22px;
  line-height: 1;
}

.floating-palette {
  width: 240px;
  border-radius: 8px;
  background: rgba(12, 18, 23, 0.92);
}

.side-panel {
  top: 14px;
  right: 14px;
  bottom: 14px;
  max-width: calc(100% - 28px);
  border-radius: 8px;
}

@media (max-width: 1099px) {
  .workflow-explorer.compact-layout .workflow-sidebar {
    position: absolute;
    top: 14px;
    left: 14px;
    bottom: 14px;
    width: min(340px, calc(100vw - 28px));
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.07);
    box-shadow: 0 30px 60px rgba(0, 0, 0, 0.34);
    z-index: 25;
  }

  .workflow-explorer.compact-layout.sidebar-collapsed .workflow-sidebar {
    transform: translateX(calc(-100% - 20px));
    opacity: 0;
    pointer-events: none;
  }

  .workflow-toolbar {
    flex-direction: column;
    align-items: stretch;
    padding: 6px 8px;
  }

  .toolbar-left,
  .toolbar-right {
    width: 100%;
  }

  .toolbar-right {
    justify-content: flex-start;
  }

  .workflow-menu-trigger,
  .workflow-menu-panel {
    width: min(100%, 280px);
    max-width: 100%;
  }
}

@media (max-width: 640px) {
  .workflow-toolbar {
    padding: 12px 14px;
  }

  .wf-title-input {
    font-size: 14px;
    width: 100%;
  }

  .workflow-menu {
    width: 100%;
  }

  .workflow-menu-trigger,
  .workflow-menu-panel {
    width: 100%;
    max-width: 100%;
  }

  .toolbar-right .btn {
    flex: 1 1 calc(50% - 8px);
  }

  .side-panel {
    top: 10px;
    right: 10px;
    bottom: 10px;
    width: calc(100% - 20px);
  }

  .node-picker-container {
    top: 12px;
    left: 12px;
  }
}

/* Minimalist workflow refresh */
.workflow-explorer {
  background:
    radial-gradient(circle at top left, rgba(213, 229, 183, 0.08), transparent 20%),
    radial-gradient(circle at top right, rgba(183, 204, 199, 0.08), transparent 18%),
    linear-gradient(180deg, #0c1111 0%, #121717 100%);
}

.workflow-sidebar {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent 18%),
    rgba(15, 19, 19, 0.92);
  border-right: 1px solid rgba(255, 255, 255, 0.06);
}

.sidebar-header {
  padding: 14px;
}

.sidebar-header h2 {
  font-size: 0.98rem;
  letter-spacing: -0.03em;
}

.sidebar-kicker {
  color: rgba(151, 165, 159, 0.9);
}

.workflow-list {
  padding: 8px;
}

.workflow-item {
  margin-bottom: 8px;
  padding: 11px 12px;
  border-radius: 14px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.04), rgba(255, 255, 255, 0.015)),
    rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.05);
}

.workflow-item:hover {
  border-color: rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.05);
}

.workflow-item.active {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.08), rgba(255, 255, 255, 0.03)),
    rgba(255, 255, 255, 0.03);
  border-color: rgba(255, 255, 255, 0.12);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
}

.wf-dot {
  background: rgba(255, 255, 255, 0.16);
}

.wf-dot.success {
  background: #b7d79a;
  box-shadow: 0 0 0 6px rgba(183, 215, 154, 0.12);
}

.wf-dot.error {
  background: #e3a2a2;
  box-shadow: 0 0 0 6px rgba(227, 162, 162, 0.12);
}

.wf-name {
  font-size: 0.84rem;
  font-weight: 600;
}

.wf-meta,
.empty-sidebar {
  color: #97a59f;
}

.workflow-toolbar {
  min-height: 56px;
  padding: 10px 12px;
  position: relative;
  z-index: 80;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent 55%),
    rgba(12, 16, 16, 0.76);
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  backdrop-filter: blur(20px);
}

.toolbar-left {
  gap: 10px;
  position: relative;
}

.workflow-menu {
  position: relative;
  flex: 0 0 auto;
  z-index: 90;
}

.workflow-menu-trigger {
  min-width: 200px;
  max-width: 280px;
  min-height: 34px;
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 0 12px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
  color: #f4f2ed;
  font: inherit;
  cursor: pointer;
}

.workflow-menu-trigger-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 600;
}

.workflow-menu-trigger-icon {
  flex-shrink: 0;
  color: #97a59f;
  font-weight: 700;
}

.workflow-menu-panel {
  position: absolute;
  top: calc(100% + 8px);
  left: 0;
  width: 280px;
  padding: 10px;
  border-radius: 18px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0.015)),
    rgba(12, 16, 16, 0.96);
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow: 0 24px 54px rgba(0, 0, 0, 0.34);
  z-index: 120;
}

.workflow-menu-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 10px;
}

.workflow-menu-kicker {
  font-size: 0.68rem;
  font-weight: 700;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: #97a59f;
}

.workflow-menu-search {
  width: 100%;
  margin-bottom: 10px;
  padding: 9px 11px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
  color: #f4f2ed;
}

.workflow-menu-list {
  max-height: 280px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.workflow-menu-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.05);
  background: rgba(255, 255, 255, 0.025);
  color: #f4f2ed;
  text-align: left;
  cursor: pointer;
}

.workflow-menu-item.active {
  background: rgba(255, 255, 255, 0.07);
  border-color: rgba(255, 255, 255, 0.1);
}

.workflow-menu-copy {
  min-width: 0;
}

.workflow-menu-name {
  font-size: 0.84rem;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.workflow-menu-meta,
.workflow-menu-empty {
  font-size: 0.72rem;
  color: #97a59f;
}

.workflow-menu-empty {
  padding: 8px 4px;
}

.toolbar-right {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.workflow-action-btn {
  /* Structural only — color comes from the global .btn-primary/.btn-success/
     .btn-danger/.btn-neutral palette in style.css (single source of truth). */
  min-width: 88px;
  justify-content: center;
}

.wf-title-input {
  width: auto;
  min-width: 160px;
  max-width: min(100%, 520px);
  font-size: 1rem;
  font-weight: 700;
  letter-spacing: -0.03em;
  padding: 0 0 2px 0;
  border: 0 !important;
  border-radius: 0 !important;
  background: transparent !important;
  box-shadow: none !important;
}

.wf-title-input:focus {
  box-shadow: none !important;
  border: 0 !important;
}

.wf-title-input::placeholder {
  color: rgba(244, 242, 237, 0.45);
}

.node-picker-trigger {
  width: 40px;
  height: 40px;
  background: linear-gradient(180deg, #f7f4ee, #e8e0d3);
  color: #0f1412;
  border: 1px solid rgba(255, 255, 255, 0.14);
  box-shadow: 0 18px 36px rgba(0, 0, 0, 0.24);
}

.node-picker-trigger:hover {
  box-shadow: 0 22px 42px rgba(0, 0, 0, 0.28);
}

.floating-palette {
  width: 228px;
  padding: 8px 6px;
  border-radius: 18px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0.015)),
    rgba(13, 18, 18, 0.94);
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow: 0 30px 60px rgba(0, 0, 0, 0.35);
}

.palette-label {
  color: #97a59f;
}

.palette-search-input {
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.08);
}

.palette-search-input:focus {
  border-color: rgba(213, 229, 183, 0.3);
}

.palette-btn {
  border-radius: 12px;
  color: rgba(244, 242, 237, 0.78);
}

.palette-btn:hover {
  transform: none;
  background: rgba(255, 255, 255, 0.05);
}

.side-panel,
.node-context-menu {
  border-radius: 18px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0.015)),
    rgba(12, 16, 16, 0.94);
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow: 0 28px 60px rgba(0, 0, 0, 0.34);
}

.panel-header,
.panel-body {
  padding: 16px;
}

.context-item {
  border-radius: 12px;
}

.c-icon {
  width: 34px;
  min-width: 34px;
  height: 24px;
  align-items: center;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.08em;
  color: #97a59f;
}

.history-item {
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.06);
}

.h-meta,
.h-node-name {
  color: #97a59f;
}

.h-node-out {
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.05);
}

.workflow-empty-state {
  gap: 14px;
}

.empty-icon {
  width: 82px;
  height: 82px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 20px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0.02)),
    rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.06);
  font-size: 0.92rem;
  font-weight: 700;
  letter-spacing: 0.22em;
  color: rgba(244, 242, 237, 0.64);
  filter: none;
}

.workflow-empty-state h3 {
  font-size: 1.2rem;
  line-height: 1.05;
  letter-spacing: -0.04em;
}

.workflow-empty-state p {
  max-width: 360px;
  color: #97a59f;
  line-height: 1.5;
}

/* Workflow settings popover (A3 error-workflow picker) */
.wf-settings-pop {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 40;
  width: 280px;
  padding: 12px;
  background: #15151a;
  border: 1px solid rgba(129, 230, 217, 0.2);
  border-radius: 10px;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
}
.wf-settings-title {
  font-size: 12px;
  font-weight: 600;
  color: #e6f1ee;
  margin-bottom: 10px;
}
.wf-settings-label {
  display: block;
  font-size: 11px;
  color: #97a59f;
  margin-bottom: 4px;
}
.wf-settings-select {
  width: 100%;
  padding: 6px 8px;
  background: #0e0e12;
  color: #e6f1ee;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 6px;
  font-size: 12px;
}
.wf-settings-select option {
  background: #15151a;
  color: #e6f1ee;
}
.wf-settings-hint {
  margin-top: 8px;
  font-size: 10.5px;
  color: #6f7d77;
  line-height: 1.45;
}
.wf-settings-divider {
  height: 1px;
  background: rgba(255, 255, 255, 0.08);
  margin: 12px 0;
}
.wf-settings-actions {
  display: flex;
  gap: 8px;
}
.wf-settings-actions .btn {
  flex: 1;
}
</style>

