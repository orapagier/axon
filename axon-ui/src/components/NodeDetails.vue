<script setup>
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'
import Pill from './Pill.vue'
import SearchableSelect from './SearchableSelect.vue'
import DataTreeNode from './DataTreeNode.vue'
import { NODE_TYPES } from '../lib/nodes.js'
import { renameNodeInExpressions, applyAccessPatterns } from '../lib/expressionUpdates.js'
import { get } from '../lib/api.js'
import { toast } from '../lib/toast.js'

const props = defineProps({
  node: { type: Object, required: true },
  nodes: { type: Array, default: () => [] }, // All nodes for cross-reference updates
  upstreamNodes: { type: Array, default: () => [] },
  downstreamNodes: { type: Array, default: () => [] },
  lastRun: { type: Object, default: null },
  workflowId: { type: String, required: true },
  executing: { type: Boolean, default: false },
  mcpDynamicProps: { type: Function, default: null },
})

const emit = defineEmits(['close', 'save', 'execute', 'delete', 'switch', 'clear-execution', 'rename', 'output-added', 'output-removed'])

const activeTab = ref('parameters')
const inputMode = ref('schema')
const outputMode = ref('json')

// Panel Resizing State
const panelWidths = ref({ left: 320, right: 400 })
const isResizing = ref(null)
const startX = ref(0)
const startWidth = ref(0)

const credentials = ref([])
const availableModels = ref([])
const availableTools = ref([])
const foveaFolderOptions = ref([
  {
    name: 'data/files',
    value: '.',
    description: 'Use all images under the root data/files folder.',
  },
])

onMounted(async () => {
  const saved = localStorage.getItem('ndv-panel-widths')
  if (saved) {
    try {
      const parsed = JSON.parse(saved)
      if (parsed.left) panelWidths.value.left = parsed.left
      if (parsed.right) panelWidths.value.right = parsed.right
    } catch (e) {}
  }
  try {
    const data = await get('/credentials')
    credentials.value = data.credentials || []
  } catch (e) {
    console.error('Failed to load credentials', e)
  }
  // Fetch available models for Axon node dropdown
  try {
    const mData = await get('/models')
    availableModels.value = (mData.models || []).filter(m => m.enabled).map(m => ({ name: m.name, value: m.name }))
    availableModels.value.unshift({ name: '(Auto-select)', value: '' })
  } catch (e) {
    console.error('Failed to load models', e)
  }
  // Fetch available tools for Axon node multi-select
  try {
    const tData = await get('/tools')
    availableTools.value = (tData.tools || []).filter(t => t.enabled !== false).map(t => ({ name: t.name, value: t.name }))
  } catch (e) {
    console.error('Failed to load tools', e)
  }
  try {
    const folderData = await get('/fovea/folders')
    const folders = (folderData.folders || []).map((folder) => ({
      name: folder === '.' ? 'data/files' : `data/files/${folder}`,
      value: folder,
      description: folder === '.' ? 'Use all images under the root data/files folder.' : 'Use all images under this folder and its subfolders.',
    }))
    if (folders.length > 0) {
      foveaFolderOptions.value = folders
    }
  } catch (e) {
    console.error('Failed to load Fovea image folders', e)
  }
})

function getCredentialsForService(service) {
  if (!credentials.value || !service) return []
  return credentials.value.filter(c => c.service && c.service.toLowerCase() === service.toLowerCase())
}

function getOptionDescription(prop, value) {
  const option = (prop?.options || []).find((opt) => opt.value === value)
  return option?.description || ''
}

function optionDisplayName(opt) {
  if (!opt) return ''
  if (opt.description) return `${opt.name} - ${opt.description}`
  return opt.name
}

function getOptionLabel(prop, value) {
  return (prop?.options || []).find((opt) => opt.value === value)?.name || value
}

function startResize(panel, event) {
  isResizing.value = panel
  startX.value = event.clientX
  startWidth.value = panelWidths.value[panel]
  document.addEventListener('mousemove', onMouseMove)
  document.addEventListener('mouseup', onMouseUp)
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
}

function onMouseMove(event) {
  if (!isResizing.value) return
  const diff = event.clientX - startX.value
  let newWidth
  if (isResizing.value === 'left') {
    newWidth = Math.max(280, startWidth.value + diff)
    newWidth = Math.min(newWidth, window.innerWidth - panelWidths.value.right - 400)
    panelWidths.value.left = newWidth
  } else if (isResizing.value === 'right') {
    newWidth = Math.max(280, startWidth.value - diff)
    newWidth = Math.min(newWidth, window.innerWidth - panelWidths.value.left - 400)
    panelWidths.value.right = newWidth
  }
}

function onMouseUp() {
  document.removeEventListener('mousemove', onMouseMove)
  document.removeEventListener('mouseup', onMouseUp)
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  localStorage.setItem('ndv-panel-widths', JSON.stringify(panelWidths.value))
  isResizing.value = null
}

// Inline Rename State
const isRenaming = ref(false)
const originalLabel = ref('')
const renameInput = ref(null)

// Variable Preview State
const focusedField = ref(null) // { name: string, collection: string, index: number, subName: string }
let blurTimeout = null
const keepPreview = ref(false)

// Expanded Nodes State
const expandedNodes = ref(new Set())
function toggleNodeCollapse(nodeId) {
  if (expandedNodes.value.has(nodeId)) {
    expandedNodes.value.delete(nodeId)
  } else {
    expandedNodes.value.add(nodeId)
  }
}

function handleFocus(event, fieldInfo) {
  if (blurTimeout) clearTimeout(blurTimeout)
  focusedField.value = fieldInfo
}

function handleBlur() {
  blurTimeout = setTimeout(() => {
    if (!keepPreview.value) {
      focusedField.value = null
    }
  }, 300)
}

function releasePreview() {
  keepPreview.value = false
  // If the input is no longer focused, close the preview
  if (document.activeElement.tagName !== 'INPUT' && document.activeElement.tagName !== 'TEXTAREA') {
    focusedField.value = null
  }
}

// ── DateTime picker helpers ───────────────────────────────────────────────────
// Converts stored ISO 8601 (e.g. "2025-06-15T09:00:00") to the value
// format expected by <input type="datetime-local"> ("2025-06-15T09:00")
function isoToLocal(val) {
  if (!val || typeof val !== 'string') return ''
  // datetime-local needs exactly "YYYY-MM-DDTHH:mm"
  return val.slice(0, 16)
}

// Converts datetime-local value ("2025-06-15T09:00") back to ISO 8601
// ("2025-06-15T09:00:00") which the calendar API expects
function localToIso(val) {
  if (!val) return ''
  return val.length === 16 ? val + ':00' : val
}

function hasExpression(val) {
  // Recognize both the legacy {{ }} wrapped form and the bare n8n-style
  // $node["Name"].field form emitted by drag-and-drop.
  if (typeof val !== 'string') return false
  if (val.includes('{{')) return true
  return /\$node(\.|\[['"])[A-Za-z0-9 _-]/.test(val)
}

const focusedRawValue = computed(() => {
  if (!focusedField.value) return null
  const { name, collection, index, subName } = focusedField.value
  
  if (collection) {
    const coll = props.node.data.config[collection]?.parameters || []
    return coll[index]?.[subName] || ''
  } else {
    return props.node.data.config[name] || ''
  }
})

const focusedValue = computed(() => {
  const raw = focusedRawValue.value
  if (!raw || !hasExpression(raw)) return null

  // null when unresolved -> preview shows "(Waiting for data...)" rather than
  // echoing the raw {{ }} back at the user.
  return resolveExpression(raw)
})

function isFieldFocused(fieldInfo) {
  if (!focusedField.value) return false
  return focusedField.value.name === fieldInfo.name && 
         focusedField.value.collection === fieldInfo.collection &&
         focusedField.value.index === fieldInfo.index &&
         focusedField.value.subName === fieldInfo.subName
}

function startRename() {
  originalLabel.value = props.node.data.label
  isRenaming.value = true
  nextTick(() => {
    if (renameInput.value) renameInput.value.focus()
  })
}

const webhookUrl = computed(() => {
  if (props.node.data.node_type !== 'trigger' && props.node.data.node_type !== 'stimulus') return ''
  return `${window.location.origin}/webhook/external/${props.node.id}`
})

async function copyWebhookUrl() {
  try {
    await navigator.clipboard.writeText(webhookUrl.value)
    // No toast system visible, quiet copy
  } catch (err) {
    console.error('Failed to copy webhook URL', err)
  }
}

// Persist a settings toggle immediately. The settings checkboxes bind directly
// to node.data via v-model, so the in-memory state updates instantly — but
// without emitting save here the change never reaches the backend, which is why
// toggling Enable felt like it "didn't take effect". Keep `disabled` in sync so
// the canvas strike-through reflects the new state right away.
function onSettingsChange() {
  props.node.data.disabled = props.node.data.enabled === false
  emit('save')
}

function finishRename() {
  const oldLabel = originalLabel.value;
  isRenaming.value = false;
  let newLabel = props.node.data.label?.trim();
  if (!newLabel) {
    newLabel = nodeDefinition.value.displayName || 'Neuron';
  }

  // Enforce uniqueness (n8n style)
  let finalLabel = newLabel;
  let counter = 1;
  const otherNodes = props.nodes.filter(n => n.id !== props.node.id);
  
  while (otherNodes.some(n => n.data.label === finalLabel)) {
    finalLabel = `${newLabel} ${counter++}`;
  }
  
  // Remember an explicit rename so auto-labelling won't clobber it; a blank
  // entry clears the flag and reverts to the action-derived label below.
  props.node.data.labelEdited = !!props.node.data.label?.trim();
  props.node.data.label = finalLabel;
  props.node.data.name = finalLabel;

  if (oldLabel !== finalLabel) {
    if (oldLabel && props.nodes.length > 0) {
      console.log(`[Rename] Syncing expressions: "${oldLabel}" -> "${finalLabel}"`);
      renameNodeInExpressions(props.nodes, oldLabel, finalLabel);
    }
    // Mirror the new label into Vue Flow's internal store — the canvas keeps its
    // own node objects, so mutating the page array alone never reaches it (the
    // label only updated after a reload). Then persist all changed nodes.
    emit('rename', { id: props.node.id, name: finalLabel });
    emit('save');
  }

  // Name cleared → fall back to an automatic, action-derived label.
  if (!props.node.data.labelEdited) applyAutoLabel();
}

// Logic for current node
const nodeResult = computed(() => {
  if (!props.lastRun) return null
  const run = props.lastRun.node_results ? props.lastRun : props.lastRun.result
  if (!run || !run.node_results) return null
  const nid = String(props.node.id)
  return run.node_results.find(r => String(r.node_id) === nid) || null
})

const errorDisplay = computed(() => {
  const err = nodeResult.value?.error
  if (!err) return null
  if (typeof err === 'string') return { message: err }
  return err
})

function getNodeResult(nodeId) {
  if (!props.lastRun) return null
  const run = props.lastRun.node_results ? props.lastRun : props.lastRun.result
  if (!run || !run.node_results) return null
  const nid = String(nodeId)
  return run.node_results.find(r => String(r.node_id) === nid) || null
}

function isLikelyImageCandidate(rawValue, keyPath = '') {
  if (typeof rawValue !== 'string') return false
  const value = rawValue.trim()
  if (!value) return false

  const key = String(keyPath || '').toLowerCase()
  const hasImageExtension = /\.(png|jpe?g|webp|gif|amp|tiff?)(?:$|[?#])/i.test(value)
  const isImageDataUri = value.startsWith('data:image/')
  const isHttp = /^https?:\/\//i.test(value)
  const isPathLike = value.includes('/') || value.includes('\\')
  const hasImageHint = ['image', 'photo', 'picture', 'thumbnail', 'preview', 'src', 'download', 'local_path', 'path', 'portrait', 'landscape'].some((hint) => key.includes(hint))
  const hasProviderHint = /pexels|unsplash|pixabay|images\./i.test(value)

  return isImageDataUri || hasImageExtension || ((isHttp || isPathLike) && (hasImageHint || hasProviderHint))
}

function buildImageOptionLabel(value) {
  const raw = String(value).trim()
  if (!raw) return ''
  const tail = raw.split(/[\\/]/).pop()?.split('?')[0] || raw
  return tail.length > 48 ? `${tail.slice(0, 45)}...` : tail
}

function collectUpstreamImageOptions(value, seen, options, nodeLabel, keyPath = 'output') {
  if (value == null) return

  if (typeof value === 'string') {
    if (!isLikelyImageCandidate(value, keyPath)) return
    const normalized = value.trim()
    if (seen.has(normalized)) return
    seen.add(normalized)
    options.push({
      name: buildImageOptionLabel(normalized),
      value: normalized,
      description: `${nodeLabel} -> ${keyPath}`,
    })
    return
  }

  if (Array.isArray(value)) {
    value.forEach((entry, index) => collectUpstreamImageOptions(entry, seen, options, nodeLabel, `${keyPath}[${index}]`))
    return
  }

  if (typeof value === 'object') {
    Object.entries(value).forEach(([key, entry]) => {
      const nextPath = keyPath ? `${keyPath}.${key}` : key
      collectUpstreamImageOptions(entry, seen, options, nodeLabel, nextPath)
    })
  }
}

const foveaUpstreamImageOptions = computed(() => {
  const options = []
  const seen = new Set()

  for (const upstreamNode of props.upstreamNodes || []) {
    const result = getNodeResult(upstreamNode.id)
    if (!result) continue
    const nodeLabel = upstreamNode?.data?.label || upstreamNode?.id || 'Upstream node'
    collectUpstreamImageOptions(result.output, seen, options, nodeLabel)
  }

  return options
})

function formatOutput(data) {
  if (!data) return 'No output'
  
  // If it's an object, stringify it
  if (typeof data === 'object') {
    // Deeply try to parse strings that look like JSON inside the object
    const cleaned = JSON.parse(JSON.stringify(data), (key, value) => {
      if (typeof value === 'string' && (value.startsWith('{') || value.startsWith('['))) {
        try {
          return JSON.parse(value)
        } catch (e) {
          return value
        }
      }
      return value
    })
    return JSON.stringify(cleaned, null, 2)
  }
  
  // If it's a string that looks like JSON, try to parse it
  if (typeof data === 'string' && (data.trim().startsWith('{') || data.trim().startsWith('['))) {
    try {
      const parsed = JSON.parse(data)
      return JSON.stringify(parsed, null, 2)
    } catch (e) {
      return data
    }
  }
  
  return String(data)
}

function getNodeIcon(nodeType) {
  return NODE_TYPES[nodeType]?.icon || '📦'
}

function isImageUrl(icon) {
  if (!icon) return false
  return icon.startsWith('data:image') || icon.startsWith('http') || icon.startsWith('/') || icon.startsWith('blob:')
}


function getUpstreamData(nodeId) {
  const res = getNodeResult(nodeId)
  // An errored result is not reusable data: show its error (via getUpstreamError)
  // and drop the "Has Data" badge, so the panel matches Execute Step's decision to
  // re-run the whole chain rather than reuse this upstream output.
  if (!res || res.error || !res.output) return null
  return res.output
}

function getUpstreamError(nodeId) {
  const res = getNodeResult(nodeId)
  if (!res || !res.error) return null
  return res.error
}

function getSchema(data, path = '', currentDepth = 0) {
  if (!data || typeof data !== 'object' || data === null) return []
  let fields = []

  for (const [key, value] of Object.entries(data)) {
    // Always build a dot-separated path (e.g. "messages.0.subject"). Numeric keys
    // from arrays become plain segments. This resolves cleanly both in the live
    // preview below and in the backend JSON-pointer resolver, and avoids the broken
    // ".data.[0]" form that the old bracket-indexed roots produced.
    const fullPath = path ? `${path}.${key}` : `${key}`

    const type = Array.isArray(value) ? 'array' : typeof value
    const isObject = typeof value === 'object' && value !== null && !Array.isArray(value)
    
    // Treat Arrays as expandable objects like typical JSON viewers do
    const isExpandable = isObject || Array.isArray(value)

    fields.push({
      key,
      fullPath,
      type,
      depth: currentDepth,
      value: isObject ? '{...}' : (Array.isArray(value) ? `[${value.length}]` : value),
      isObject: Object.keys(value || {}).length > 0 && isExpandable, // Avoid expanding empty objects/arrays
      children: isExpandable ? getSchema(value, fullPath, currentDepth + 1) : []
    })
  }
  return fields
}

function onDragStart(event, nodeId, fullPath) {
  // Bare expression form (no {{ }}), n8n-style.
  event.dataTransfer.setData('variable', `$node["${nodeId}"].data.${fullPath}`)
  event.dataTransfer.effectAllowed = 'copy'
}

// Walk a value by a path that may use dot notation ("messages.0.subject")
// or legacy bracket notation ("messages[0].subject"). Mirrors how the backend
// JSON-pointer resolver navigates upstream output.
function getByPath(root, path) {
  if (!path) return root
  const parts = String(path)
    .replace(/\[(\w+)\]/g, '.$1') // items[0] -> items.0
    .split('.')
    .filter((p) => p !== '')
  let current = root
  for (const p of parts) {
    if (current === null || current === undefined) return undefined
    current = current[p]
  }
  return current
}

// Resolve $node["Label"].data.path expressions against the most recent
// upstream output, so the field shows the real value just like n8n. Accepts
// the legacy {{ }}-wrapped form AND the bare form emitted by drag-and-drop,
// plus the .data / .output / .json aliases and a bare reference (whole output).
// Returns null when nothing could be resolved (preview shows "Waiting for data...").
function resolveExpression(expr) {
  if (!expr || typeof expr !== 'string') return null
  if (!hasExpression(expr)) return null

  // Matches both: {{ $node["X"].data.path }} and bare $node["X"].data.path
  // The leading {{ and trailing }} (plus their inner padding) are matched as a
  // single optional unit so that, for the bare form, a trailing space the user
  // typed after the token is NOT swallowed into the match (and thus erased on
  // replace). Previously a lone `\s*` here ate that space, making the resolved
  // value stick to the next word unless the user wrapped it in {{ }}.
  const regex = /(?:\{\{\s*)?\$?node\[['"](.+?)['"]\]\.(?:data|output|json)\.?([a-zA-Z0-9_\-\.\[\]]*)(?:\s*\}\})?/g
  let resolved = expr
  let match

  while ((match = regex.exec(expr)) !== null) {
    const nodeLabel = match[1]
    const path = match[2] || ''

    const un = props.upstreamNodes.find(
      (n) => (n.data.label || '').toLowerCase() === nodeLabel.toLowerCase()
    )
    if (!un) continue

    const data = getUpstreamData(un.id)
    if (data === null || data === undefined) continue

    const value = getByPath(data, path)
    if (value !== undefined) {
      resolved = resolved.replace(
        match[0],
        typeof value === 'object' ? JSON.stringify(value) : String(value)
      )
    }
  }

  // Nothing changed -> nothing resolved (upstream not executed yet, or bad path).
  return resolved === expr ? null : resolved
}

// Resolved value for a field, for the always-visible inline preview (n8n style).
function inlineResolved(raw) {
  if (!hasExpression(raw)) return null
  return resolveExpression(raw)
}

function onDrop(event, propName) {
  const token = event.dataTransfer.getData('variable')
  if (token) {
    const currentVal = props.node.data.config[propName] || ''
    const el = event.target || event.srcElement
    if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && typeof el.selectionStart === 'number') {
      const pos = el.selectionStart
      props.node.data.config[propName] = currentVal.substring(0, pos) + token + currentVal.substring(pos)
    } else {
      props.node.data.config[propName] = currentVal + token
    }
  }
}

function onDropCollection(event, propName, index, fieldName) {
  const token = event.dataTransfer.getData('variable')
  if (token) {
    const coll = props.node.data.config[propName]
    if (coll && coll.parameters && coll.parameters[index]) {
      const currentVal = coll.parameters[index][fieldName] || ''
      const el = event.target || event.srcElement
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && typeof el.selectionStart === 'number') {
        const pos = el.selectionStart
        coll.parameters[index][fieldName] = currentVal.substring(0, pos) + token + currentVal.substring(pos)
      } else {
        coll.parameters[index][fieldName] = currentVal + token
      }
    }
  }
}

// A fixedCollection whose items each map to a dynamic output handle (the Switch
// node's routing rules). Adding/removing such an item shifts every later output
// index, so connected edges must be re-indexed by the parent — see emits below.
function isDynamicOutputCollection(propName) {
  return !!NODE_TYPES[props.node.data.node_type]?.dynamicOutputs && propName === 'rules'
}

function addCollectionItem(propName, options) {
  if (!props.node.data.config[propName]) {
    props.node.data.config[propName] = { parameters: [] }
  }
  if (!props.node.data.config[propName].parameters) {
    props.node.data.config[propName].parameters = []
  }
  const newItem = {}
  options.forEach(opt => {
    newItem[opt.name] = opt.default !== undefined ? opt.default : ''
  })
  props.node.data.config[propName].parameters.push(newItem)
  if (isDynamicOutputCollection(propName)) {
    // New rule takes this index; the trailing Default output shifts down by one,
    // so the parent bumps any edge at or past it to keep wiring intact.
    const index = props.node.data.config[propName].parameters.length - 1
    emit('output-added', { nodeId: props.node.id, index })
  }
}

function removeCollectionItem(propName, index) {
  const coll = props.node.data.config[propName]
  if (coll && coll.parameters) {
    coll.parameters.splice(index, 1)
    if (isDynamicOutputCollection(propName)) {
      // Drop the deleted rule's own edge and shift later outputs up by one.
      emit('output-removed', { nodeId: props.node.id, index })
    }
  }
}

// Filter an options-field's choices by another field's current value.
// A subProp may declare `filterBy: 'dataType'`; each option may declare
// `show: ['string','number',...]`. Used by IF/Switch so the operator list
// only shows operators valid for the chosen data type (n8n parity). The
// filter key is read from the current collection row first, then falls back
// to the node config (Switch keeps a single top-level dataType).
function filteredOptions(subProp, item = null) {
  const opts = subProp.options || []
  if (!subProp.filterBy) return opts
  let current = item && item[subProp.filterBy] !== undefined
    ? item[subProp.filterBy]
    : props.node.data.config[subProp.filterBy]
  return opts.filter(o => !o.show || (current !== undefined && o.show.includes(current)))
}

const nodeDefinition = computed(() => {
  const type = props.node.data.node_type === 'trigger' ? 'trigger' : props.node.data.node_type
  const base = NODE_TYPES[type] || { properties: [], displayName: 'Neuron', icon: '📦' }

  // For Axon and Classifier nodes, inject dynamic options into model/tools properties
  if ((type === 'axon' || type === 'classifier') && base.properties) {
    const enriched = { ...base, properties: base.properties.map(p => {
      if (p.name === 'model' && p.type === 'options') {
        return { ...p, options: availableModels.value, searchable: true }
      }
      if (p.name === 'tools' && p.type === 'multiOptions') {
        return { ...p, options: availableTools.value }
      }
      return p
    })}
    return enriched
  }

  if (type === 'fovea' && base.properties) {
    return {
      ...base,
      properties: base.properties.map((p) => {
        if (p.name === 'image_folder' && p.type === 'options') {
          return { ...p, options: foveaFolderOptions.value, searchable: true }
        }
        if (p.name === 'images' && p.type === 'multiOptions') {
          return { ...p, options: foveaUpstreamImageOptions.value }
        }
        return p
      }),
    }
  }
  
  // For MCP nodes, merge dynamic properties from the selected tool's schema
  if (type.startsWith('mcp') && props.mcpDynamicProps) {
    const toolName = props.node.data.config?.tool_name
    if (toolName) {
      const dynProps = props.mcpDynamicProps(toolName)
      if (dynProps.length > 0) {
        return {
          ...base,
          properties: [...base.properties, ...dynProps],
        }
      }
    }
  }
  return base
})

const tableData = computed(() => {
  const output = nodeResult.value?.output
  const file = nodeResult.value?.file
  if (file && !output) return [file]
  if (!output) return []
  const data = output.items || output.body || (Array.isArray(output) ? output : [output])
  return Array.isArray(data) ? data : [data]
})

function shouldShowProperty(prop, item = null) {
  if (!prop.displayOptions?.show) return true
  for (let [key, values] of Object.entries(prop.displayOptions.show)) {
    // Strip leading / if present (n8n style for parent/root path)
    if (key.startsWith('/')) key = key.substring(1)
    
    let current = undefined;
    if (item && item[key] !== undefined) {
      current = item[key]
    } else {
      current = props.node.data.config[key]
    }
    
    // Alias bridge: if key is contentType but we have bodyContentType, use that
    if (current === undefined && key === 'contentType') {
      current = props.node.data.config['bodyContentType']
    }
    
    if (current === undefined && !item) {
      current = props.node.data[key]
    }

    // Value bridge: if config has "form-data" but we look for "multipart-form-data"
    const matches = values.includes(current) || 
                   (current === 'form-data' && values.includes('multipart-form-data')) ||
                   (current === 'multipart-form-data' && values.includes('form-data'))

    if (!matches) return false
  }

  if (prop.displayOptions?.hide) {
    for (let [key, values] of Object.entries(prop.displayOptions.hide)) {
      if (key.startsWith('/')) key = key.substring(1)
      
      let current = undefined;
      if (item && item[key] !== undefined) {
        current = item[key]
      } else {
        current = props.node.data.config[key]
      }
      
      if (current === undefined && !item) {
        current = props.node.data[key]
      }
      
      const isNotEmpty = current !== undefined && current !== null && current !== '';
      const matches = values.includes(current) || (values.includes('NOT_EMPTY') && isNotEmpty);
      
      if (matches) return false
    }
  }

  return true
}

async function copyErrorDetails() {
  const errorObj = errorDisplay.value
  if (!errorObj) return
  
  const text = `Error in ${props.node.data.label}\n\nMessage:\n${errorObj.message}\n\nStack:\n${errorObj.stack || 'No stack trace available'}`
  try {
    await navigator.clipboard.writeText(text)
  } catch (err) {
    console.error('Failed to copy error details', err)
  }
}

async function copyOutput() {
  const data = nodeResult.value?.output
  if (!data) return
  
  const text = formatOutput(data)
  try {
    await navigator.clipboard.writeText(text)
  } catch (err) {
    console.error('Failed to copy output', err)
  }
}
// ── Expandable input editor ──────────────────────────────────────────────────
// Any text/number/expression field can be opened in a roomy overlay to read or
// edit its full value, instead of squinting at a cramped single-line box. We bind
// by getter/setter closures so the same modal serves top-level config fields and
// nested collection rows alike — writes flow straight back through v-model's
// target, keeping autosave and expression previews in sync.
const expandedInput = ref(null) // { title, value, set }
const expandedInputRef = ref(null)
function openExpandedInput(title, value, setter) {
  expandedInput.value = { title: title || 'Value', value: value == null ? '' : String(value), set: setter }
  nextTick(() => { expandedInputRef.value?.focus() })
}
function updateExpandedInput(event) {
  if (!expandedInput.value) return
  const v = event.target.value
  expandedInput.value.value = v
  expandedInput.value.set(v)
}
// Accept a variable token dragged from the INPUT panel, inserting it at the
// cursor (or appending) just like the inline fields do.
function onExpandedDrop(event) {
  if (!expandedInput.value) return
  const token = event.dataTransfer.getData('variable')
  if (!token) return
  const el = event.target
  let v = expandedInput.value.value || ''
  if (el && typeof el.selectionStart === 'number') {
    const pos = el.selectionStart
    v = v.substring(0, pos) + token + v.substring(pos)
  } else {
    v = v + token
  }
  expandedInput.value.value = v
  expandedInput.value.set(v)
}
function closeExpandedInput() {
  expandedInput.value = null
}

const showCurlModal = ref(false)
const curlInput = ref('')

function openCurlImport() {
  curlInput.value = ''
  showCurlModal.value = true
}

// Tokenize a shell-style command line, honoring single/double quotes,
// $'...' ANSI-C quoting, and backslash escapes. Line continuations are
// stripped beforehand. Returns an array of unquoted argument strings.
function tokenizeShell(input) {
  const tokens = []
  let cur = ''
  let has = false
  let i = 0
  const n = input.length
  const ansiUnescape = (s) =>
    s.replace(/\\(n|t|r|\\|'|"|a|b|f|v|0)/g, (_, ch) => ({
      n: '\n', t: '\t', r: '\r', '\\': '\\', "'": "'", '"': '"',
      a: '\x07', b: '\b', f: '\f', v: '\v', '0': '\0',
    }[ch]))
  while (i < n) {
    const c = input[i]
    // $'...' / $"..." — drop the leading $ and parse as a quote
    if (c === '$' && (input[i + 1] === "'" || input[i + 1] === '"')) { i++; continue }
    if (c === "'") {
      has = true; i++
      let raw = ''
      while (i < n && input[i] !== "'") { raw += input[i]; i++ }
      i++ // closing '
      // If this single-quoted run was introduced by $'...', apply ANSI-C unescaping.
      cur += input[i - raw.length - 2] === '$' ? ansiUnescape(raw) : raw
      continue
    }
    if (c === '"') {
      has = true; i++
      while (i < n && input[i] !== '"') {
        if (input[i] === '\\' && i + 1 < n && '"\\$`\n'.includes(input[i + 1])) {
          if (input[i + 1] !== '\n') cur += input[i + 1]
          i += 2
        } else { cur += input[i]; i++ }
      }
      i++ // closing "
      continue
    }
    if (c === '\\') { // escape next char outside quotes
      if (i + 1 < n) { cur += input[i + 1]; has = true; i += 2 } else { i++ }
      continue
    }
    if (/\s/.test(c)) {
      if (has) { tokens.push(cur); cur = ''; has = false }
      i++
      continue
    }
    cur += c; has = true; i++
  }
  if (has) tokens.push(cur)
  return tokens
}

function isJsonString(s) {
  try { JSON.parse(s); return true } catch { return false }
}

// Parse "a=1&b=2" (urlencoded) into [{name,value}], decoding percent-escapes
// so the backend can re-encode cleanly.
function parseUrlencodedPairs(str) {
  const out = []
  for (const part of str.split('&')) {
    if (!part) continue
    const eq = part.indexOf('=')
    const rawName = eq === -1 ? part : part.slice(0, eq)
    const rawValue = eq === -1 ? '' : part.slice(eq + 1)
    const dec = (v) => { try { return decodeURIComponent(v.replace(/\+/g, ' ')) } catch { return v } }
    out.push({ name: dec(rawName), value: dec(rawValue) })
  }
  return out
}

function importCurl() {
  if (!curlInput.value) return

  // Strip line continuations ( \<newline> for bash, ^<newline> for cmd ) so the
  // command becomes a single logical line before tokenizing.
  const normalized = curlInput.value
    .replace(/\\\r?\n/g, ' ')
    .replace(/\^\r?\n/g, ' ')
    .trim()

  const tokens = tokenizeShell(normalized)
  if (tokens[0] && tokens[0].toLowerCase() === 'curl') tokens.shift()

  let method = ''
  let basicAuth = ''
  let insecure = false
  const positionals = []
  const rawHeaders = []   // "Name: value" strings
  const dataParts = []    // -d / --data*
  const urlencodeParts = [] // --data-urlencode
  const formParts = []    // -F / --form (multipart)
  let explicitUrl = ''

  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i]
    const eq = t.indexOf('=')
    const flag = t.startsWith('--') && eq !== -1 ? t.slice(0, eq) : t
    const inlineVal = t.startsWith('--') && eq !== -1 ? t.slice(eq + 1) : null
    const val = () => (inlineVal !== null ? inlineVal : tokens[++i])

    switch (flag) {
      case '-X': case '--request': method = (val() || '').toUpperCase(); break
      case '--url': explicitUrl = val() || ''; break
      case '-H': case '--header': { const v = val(); if (v) rawHeaders.push(v); break }
      case '-d': case '--data': case '--data-raw':
      case '--data-binary': case '--data-ascii': { const v = val(); if (v != null) dataParts.push(v); break }
      case '--data-urlencode': { const v = val(); if (v != null) urlencodeParts.push(v); break }
      case '-F': case '--form': { const v = val(); if (v) formParts.push(v); break }
      case '-u': case '--user': basicAuth = val() || ''; break
      case '-b': case '--cookie': { const v = val(); if (v) rawHeaders.push('Cookie: ' + v); break }
      case '-A': case '--user-agent': { const v = val(); if (v) rawHeaders.push('User-Agent: ' + v); break }
      case '-e': case '--referer': { const v = val(); if (v) rawHeaders.push('Referer: ' + v); break }
      case '-k': case '--insecure': insecure = true; break
      // No-arg flags we can safely ignore
      case '--compressed': case '-L': case '--location': case '-s': case '--silent':
      case '-i': case '--include': case '-g': case '--globoff': case '-v': case '--verbose':
      case '-#': case '--progress-bar': case '-S': case '--show-error': case '-f': case '--fail':
        break
      default:
        if (flag.startsWith('-') && flag !== '-') {
          // Unknown flag that likely carries a value — consume it so the value
          // isn't mistaken for the URL. Skip only if the next token isn't a flag.
          if (inlineVal === null && tokens[i + 1] != null && !tokens[i + 1].startsWith('-')) i++
        } else {
          positionals.push(t)
        }
    }
  }

  // URL: explicit --url wins, else first http(s) positional, else first positional.
  const url = explicitUrl ||
    positionals.find((p) => /^https?:\/\//i.test(p)) ||
    positionals[0] || ''

  if (!url) {
    toast('Couldn’t find a URL in that cURL command.', false)
    return
  }

  const config = props.node.data.config
  config.url = url

  // Headers → {name,value}; capture Content-Type to drive the body mapping.
  const headers = []
  let contentTypeHeader = ''
  for (const h of rawHeaders) {
    const idx = h.indexOf(':')
    if (idx === -1) continue
    const name = h.slice(0, idx).trim()
    const value = h.slice(idx + 1).trim()
    if (!name) continue
    if (name.toLowerCase() === 'content-type') contentTypeHeader = value.toLowerCase()
    headers.push({ name, value })
  }

  const hasBody = dataParts.length > 0 || urlencodeParts.length > 0 || formParts.length > 0
  // curl defaults to POST when a body is present and no method was given.
  config.method = method || (hasBody ? 'POST' : 'GET')

  // Decide body content type and shape. We strip the Content-Type header for
  // managed types (json/form/multipart) because the backend serializer sets the
  // correct one — crucially the multipart boundary, which a bare header would clobber.
  let bodySummary = 'no body'
  let dropContentTypeHeader = false
  if (hasBody) {
    config.sendBody = true
    if (formParts.length > 0) {
      // multipart/form-data — values like name=value, files like name=@/path
      config.contentType = 'multipart-form-data'
      config.specifyBody = 'keypair'
      config.bodyParameters = {
        parameters: formParts.map((f) => {
          const eq = f.indexOf('=')
          const name = eq === -1 ? f : f.slice(0, eq)
          let value = eq === -1 ? '' : f.slice(eq + 1)
          if (value.startsWith('@')) {
            return { name, value: value.slice(1).split(';')[0], parameterType: 'formBinaryData' }
          }
          return { name, value, parameterType: 'formData' }
        }),
      }
      dropContentTypeHeader = true
      bodySummary = `${config.bodyParameters.parameters.length}-field form-data`
    } else if (urlencodeParts.length > 0 && dataParts.length === 0) {
      config.contentType = 'form-urlencoded'
      config.specifyBody = 'keypair'
      config.bodyParameters = {
        parameters: urlencodeParts.map((p) => {
          const eq = p.indexOf('=')
          return { name: eq === -1 ? p : p.slice(0, eq), value: eq === -1 ? '' : p.slice(eq + 1) }
        }),
      }
      dropContentTypeHeader = true
      bodySummary = 'urlencoded body'
    } else {
      const data = [...dataParts, ...urlencodeParts].join('&')
      const ctIsJson = contentTypeHeader.includes('json')
      const ctIsForm = contentTypeHeader.includes('x-www-form-urlencoded')
      const looksUrlencoded = /^[^=&\s]+=[^&]*(&[^=&\s]+=[^&]*)*$/.test(data)
      const looksJson = /^\s*[[{]/.test(data) && isJsonString(data)

      if (ctIsForm || (!contentTypeHeader && looksUrlencoded && !looksJson)) {
        config.contentType = 'form-urlencoded'
        config.specifyBody = 'keypair'
        config.bodyParameters = { parameters: parseUrlencodedPairs(data) }
        dropContentTypeHeader = true
        bodySummary = 'urlencoded body'
      } else if (looksJson || (ctIsJson && isJsonString(data))) {
        config.contentType = 'json'
        config.specifyBody = 'json'
        config.jsonBody = data
        dropContentTypeHeader = true
        bodySummary = 'JSON body'
      } else {
        // Raw body (text/xml/etc., or a Content-Type:json that isn't valid JSON).
        // Keep the Content-Type header so the exact type is sent, and mirror it
        // into rawContentType for the backend's raw path.
        config.contentType = 'raw'
        config.specifyBody = 'string'
        config.body = data
        if (contentTypeHeader) config.rawContentType = contentTypeHeader
        bodySummary = 'raw body'
      }
    }
  } else {
    config.sendBody = false
  }

  // Apply headers (minus the Content-Type we mapped to the body type).
  const finalHeaders = dropContentTypeHeader
    ? headers.filter((h) => h.name.toLowerCase() !== 'content-type')
    : headers
  if (finalHeaders.length > 0) {
    config.sendHeaders = true
    config.specifyHeaders = 'keypair'
    config.headerParameters = { parameters: finalHeaders }
  } else {
    config.sendHeaders = false
    config.headerParameters = { parameters: [] }
  }

  // Basic auth via -u user:pass
  if (basicAuth) {
    const idx = basicAuth.indexOf(':')
    config.authentication = 'genericCredentialType'
    config.genericAuthType = 'httpBasicAuth'
    config.user = idx === -1 ? basicAuth : basicAuth.slice(0, idx)
    config.password = idx === -1 ? '' : basicAuth.slice(idx + 1)
  }

  // -k / --insecure → ignore SSL errors (lives in the Options collection)
  if (insecure) {
    if (!config.options || typeof config.options !== 'object') config.options = {}
    config.options.allowUnauthorizedCerts = true
  }

  showCurlModal.value = false
  emit('save')

  const parts = [config.method, getHostnameSafe(url)]
  if (finalHeaders.length) parts.push(`${finalHeaders.length} header${finalHeaders.length > 1 ? 's' : ''}`)
  parts.push(bodySummary)
  toast(`Imported cURL · ${parts.join(' · ')}`)
}

function getHostnameSafe(url) {
  try { return new URL(url).hostname } catch { return url }
}
function normalizeConfig() {
  if (!props.node?.data?.config || !nodeDefinition.value) return
  nodeDefinition.value.properties.forEach(p => {
    if (props.node.data.config[p.name] === undefined) {
      if (p.type === 'collection') {
        props.node.data.config[p.name] = {}
      } else if (p.type === 'fixedCollection') {
        props.node.data.config[p.name] = { parameters: [] }
      } else if (p.type === 'multiOptions') {
        props.node.data.config[p.name] = p.default || []
      } else {
        props.node.data.config[p.name] = p.default
      }
    }
  })

  // Backward compatibility for Circadian/Stimulus nodes using legacy cron_nl
  if ((props.node.data.node_type === 'circadian' || props.node.data.node_type === 'stimulus') && props.node.data.config.schedules?.parameters) {
    props.node.data.config.schedules.parameters.forEach(p => {
      if (p.cron_nl && !p.mode) {
        let text = p.cron_nl.toLowerCase();
        
        if (text === 'hourly' || text === 'every hour') {
          p.mode = 'hours'; p.value = 1;
        } else if (text === 'daily') {
          p.mode = 'days'; p.value = 1;
        } else if (text.startsWith('every ')) {
          const match = text.match(/every\s+(\d+)\s+(min|hour|day)s?/);
          if (match) {
            p.value = parseInt(match[1]) || 1;
            const unit = match[2];
            if (unit === 'min') p.mode = 'minutes';
            else if (unit === 'hour') p.mode = 'hours';
            else if (unit === 'day') p.mode = 'days';
          } else {
            p.mode = 'custom'; p.customCron = text;
          }
        } else {
          p.mode = 'custom'; p.customCron = p.cron_nl;
        }
      }
    });
  }
}

watch(() => props.node.id, normalizeConfig, { immediate: true })

// Re-normalize when MCP tool name changes (dynamic properties change)
watch(() => props.node.data.config?.tool_name, (newTool) => {
  if (props.node.data.node_type?.startsWith('mcp') && newTool) {
    normalizeConfig()
  }
})

// ── Automatic node labels ──────────────────────────────────────────────────
// A node's label tracks its primary "action" field — operation / tool action /
// action / trigger type — so changing what a node does keeps its label honest
// (e.g. a Gmail node relabels from "Send Email" to "Add Label" when you switch
// the action). We only ever overwrite a label the user hasn't hand-edited:
// `labelEdited` guards the active session, and isAutoLabel() recognises any
// label we could have generated so this still holds after a reload, where the
// flag isn't persisted.
const ACTION_FIELDS = ['operation', 'tool_name', 'action', 'type']

function autoLabelDef() {
  const def = nodeDefinition.value
  // Legacy 'trigger' node_type has no NODE_TYPES entry (it is keyed 'stimulus').
  if ((!def?.properties || def.properties.length === 0) && props.node.data.node_type === 'trigger') {
    return NODE_TYPES.stimulus
  }
  return def
}

function primaryActionProp(def) {
  if (!def?.properties) return null
  for (const fieldName of ACTION_FIELDS) {
    const prop = def.properties.find(p => p.name === fieldName && p.type === 'options')
    if (prop) return prop
  }
  return null
}

function humanizeLabel(value) {
  const bare = String(value).split(/[.:/]/).pop() || String(value)
  const words = bare.replace(/_/g, ' ').trim()
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : String(value)
}

// Human-readable label for one value of the action field. For MCP nodes the
// dropdown "name" is the tool's long description, so derive from the tool id and
// strip the service prefix instead (mcp_gmail + gmail_send_email -> "Send email").
function deriveActionLabel(prop, value) {
  if (value === undefined || value === null || value === '') return ''
  if (prop.name === 'tool_name') {
    const nodeType = props.node.data.node_type || ''
    const service = nodeType.startsWith('mcp_') ? nodeType.slice(4) : ''
    let bare = String(value).split(/[.:/]/).pop() || String(value)
    if (service && bare.toLowerCase().startsWith(`${service.toLowerCase()}_`)) {
      bare = bare.slice(service.length + 1)
    }
    return humanizeLabel(bare)
  }
  const opt = (prop.options || []).find(o => o.value === value)
  return opt?.name || humanizeLabel(value)
}

// True when the current label is one we generated, so it is safe to replace.
function isAutoLabel(currentLabel, def, prop) {
  if (props.node.data.labelEdited) return false
  if (!currentLabel) return true
  const stripped = currentLabel.replace(/\s+\d+$/, '')
  if (stripped === (def?.displayName || 'Neuron')) return true
  if (currentLabel === 'Neuron' || currentLabel === 'MCP Tool') return true
  if (currentLabel.includes('Tool')) return true
  // Labels emitted by the previous hand-coded trigger logic.
  if (['When clicked', 'Schedule', 'Trigger', 'Scheduled', 'Gmail Monitor'].includes(currentLabel)) return true
  if (prop) {
    for (const opt of (prop.options || [])) {
      if (currentLabel === opt.value) return true            // older raw-value labels
      if (stripped === deriveActionLabel(prop, opt.value)) return true
    }
  }
  return false
}

function uniqueLabel(base, excludeId) {
  let finalLabel = base
  let counter = 1
  const others = props.nodes.filter(n => n.id !== excludeId)
  while (others.some(n => n.data.label === finalLabel)) {
    finalLabel = `${base} ${counter++}`
  }
  return finalLabel
}

// Re-derive the label from the node's action field when it is still automatic.
function applyAutoLabel() {
  const def = autoLabelDef()
  const prop = primaryActionProp(def)
  if (!prop) return
  const currentLabel = props.node.data.label
  if (!isAutoLabel(currentLabel, def, prop)) return
  const base = deriveActionLabel(prop, props.node.data.config?.[prop.name])
  if (!base) return
  const finalLabel = uniqueLabel(base, props.node.id)
  if (finalLabel === currentLabel) return
  // Follow the rename in sibling $node["..."] expressions, but only on an
  // unambiguous rename — if a counter was appended for a name collision we must
  // not hijack references that pointed at the other node.
  if (finalLabel === base && currentLabel && props.nodes.length > 0) {
    renameNodeInExpressions(props.nodes, currentLabel, finalLabel)
  }
  props.node.data.label = finalLabel
  props.node.data.name = finalLabel
  // Mirror into Vue Flow's store so the canvas shows the auto-derived label
  // without a reload (see finishRename).
  emit('rename', { id: props.node.id, name: finalLabel })
  emit('save')
}

// Heal placeholder/stale labels when a node opens, and keep the label in sync as
// its action field changes.
watch(() => props.node.id, applyAutoLabel, { immediate: true })
watch(
  () => {
    const prop = primaryActionProp(autoLabelDef())
    return prop ? props.node.data.config?.[prop.name] : undefined
  },
  applyAutoLabel,
)

// ── Autosave ────────────────────────────────────────────────────────────────
// Parameter fields bind straight to node.data.config via v-model, so on their
// own they only mutated in-memory state — edits were silently lost on reload or
// when switching workflows unless some *other* action happened to trigger a
// save. Debounce-persist any config change so editing "just saves" (n8n-style).
// Saves are silent here (no toast) — the explicit Save button and structural
// canvas actions stay loud.
let autosaveTimer = null

// Disarm across a node switch so the defaults normalizeConfig() backfills for the
// newly-opened node don't fire a spurious save. Declared BEFORE the config
// watcher so it runs first in the same flush and the guard is already false.
const autosaveArmed = ref(false)
watch(() => props.node.id, () => {
  autosaveArmed.value = false
  nextTick(() => { autosaveArmed.value = true })
}, { immediate: true })

watch(
  () => props.node.data.config,
  () => {
    if (!autosaveArmed.value) return
    if (autosaveTimer) clearTimeout(autosaveTimer)
    autosaveTimer = setTimeout(() => {
      autosaveTimer = null
      emit('save', { silent: true })
    }, 800)
  },
  { deep: true },
)

// Flush a pending autosave when the panel closes (unmounts), so a quick
// edit-then-close inside the debounce window still persists.
onUnmounted(() => {
  if (autosaveTimer) {
    clearTimeout(autosaveTimer)
    autosaveTimer = null
    emit('save', { silent: true })
  }
})

</script>

<template>
  <Teleport to="body">
    <div class="nd-overlay" @click="emit('close')" @keydown.stop @keyup.stop @copy.stop @paste.stop>
      <div class="nd-window" @click.stop>
      <!-- Floating Prev Nav -->
      <div v-if="upstreamNodes.length > 0" class="nd-floating-nav left">
      <div 
        v-for="un in upstreamNodes" 
        :key="un.id" 
        class="nav-tab" 
        @click="emit('switch', un.id)"
        :title="'Switch to ' + un.data.label"
      >
        <img v-if="isImageUrl(getNodeIcon(un.data.node_type))" :src="getNodeIcon(un.data.node_type)" class="nav-tab-icon" />
        <span v-else>{{ getNodeIcon(un.data.node_type) }}</span>
      </div>
    </div>

    <!-- Floating Next Nav -->
    <div v-if="downstreamNodes.length > 0" class="nd-floating-nav right">
      <div 
        v-for="dn in downstreamNodes" 
        :key="dn.id" 
        class="nav-tab" 
        @click="emit('switch', dn.id)"
        :title="'Switch to ' + dn.data.label"
      >
        <img v-if="isImageUrl(getNodeIcon(dn.data.node_type))" :src="getNodeIcon(dn.data.node_type)" class="nav-tab-icon" />
        <span v-else>{{ getNodeIcon(dn.data.node_type) }}</span>
      </div>
    </div>

    <div class="nd-window-inner">
      <!-- HEADER -->
      <header class="nd-header">
        <div class="nd-header-left">
          <div class="breadcrumbs">
            <span class="bc-item">Workflows</span>
            <span class="bc-sep">/</span>
            <span class="bc-item">{{ workflowId === 'new' ? 'New Workflow' : 'Workflow' }}</span>
            <span class="bc-sep">/</span>
            <span class="nd-header-icon">
              <img v-if="isImageUrl(nodeDefinition.icon)" :src="nodeDefinition.icon" class="bc-icon-img" />
              <template v-else>{{ nodeDefinition.icon }}</template>
            </span>
            
            <div class="nd-inline-rename">
              <input 
                v-if="isRenaming"
                ref="renameInput"
                type="text"
                v-model="node.data.label"
                @blur="finishRename"
                @keyup.enter="finishRename"
                @keyup.esc="finishRename"
                class="rename-input"
              />
              <span 
                v-else
                class="nd-header-title"
                @click="startRename"
                title="Click to rename"
              >
                {{ node.data.label }}
                <!-- Subtitle -->
                <span class="nd-header-subtitle" v-if="node.data.label !== nodeDefinition.displayName">
                  {{ nodeDefinition.displayName }}
                </span>
              </span>
            </div>
          </div>
        </div>
        <div class="nd-header-right">
          <button class="btn-close" @click="emit('close')">✕</button>
        </div>
      </header>

      <div class="nd-body">
        <!-- COLUMN: INPUT -->
        <section class="nd-col nd-col-left" :style="{ width: panelWidths.left + 'px' }">
          <div class="col-header">
            <span class="col-title">INPUT</span>
            <div class="col-toggles">
              <button :class="{ active: inputMode === 'schema' }" @click="inputMode = 'schema'">Schema</button>
              <button :class="{ active: inputMode === 'table' }" @click="inputMode = 'table'">Table</button>
              <button :class="{ active: inputMode === 'json' }" @click="inputMode = 'json'">JSON</button>
            </div>
          </div>
          <div class="col-content nd-input-layout">
            <div v-if="upstreamNodes.length === 0" class="data-empty">
              No input data. Connect this node to previous nodes to pass data into it.
            </div>

            <!-- Removed GUTTER for node icons -->
            <div class="nd-input-main">
              <div v-for="un in upstreamNodes" :key="un.id" class="data-node" :class="{ 'node-collapsed': !expandedNodes.has(un.id) }">
                <div class="dn-head" @click="toggleNodeCollapse(un.id)">
                  <span class="dn-chevron" :class="{ active: expandedNodes.has(un.id) }">
                    <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M8.59 16.59L13.17 12 8.59 7.41 10 6l6 6-6 6-1.41-1.41z"/></svg>
                  </span>
                  <span class="dn-label">{{ un.data.label }}</span>
                  <span v-if="getUpstreamData(un.id)" class="dn-meta">{{ inputMode === 'json' ? '{ JSON }' : 'Has Data' }}</span>
                </div>
                
                <div v-if="expandedNodes.has(un.id)" class="dn-body">
                  <div v-if="getUpstreamData(un.id)">
                    <!-- Tree Mode (Schema or JSON) -->
                    <div v-if="inputMode === 'schema' || inputMode === 'json'" class="data-tree" :class="{ 'mode-json': inputMode === 'json' }">
                      <DataTreeNode
                        v-for="field in getSchema(getUpstreamData(un.id))"
                        :key="field.fullPath"
                        :field="field"
                        :depth="0"
                        :inputMode="inputMode"
                        :nodeLabel="un.data.label"
                      />
                    </div>
                    <!-- Table fallback -->
                    <div v-else-if="inputMode === 'table'" class="data-empty">Table view coming soon</div>
                  </div>
                  <!-- Node result meta (Error/Empty) -->
                  <div v-else-if="getUpstreamError(un.id)" class="data-empty" style="color: #f87171">
                    {{ getUpstreamError(un.id).message || getUpstreamError(un.id) }}
                  </div>
                  <div v-else class="data-empty">
                    Execute previous node to see output data here
                  </div>
                </div>
              </div>
            </div>
          </div>
        </section>

        <!-- Left Resizer -->
        <div class="nd-resizer" @mousedown="startResize('left', $event)">
          <div class="resizer-handle"></div>
        </div>

        <!-- COLUMN: PARAMETERS -->
        <section class="nd-col nd-col-mid" style="flex: 1">

          <div class="col-header params-header">
            <div class="header-tabs">
              <button class="header-tab" :class="{ active: activeTab === 'parameters' }" @click="activeTab = 'parameters'">PARAMETERS</button>
              <button class="header-tab" :class="{ active: activeTab === 'settings' }" @click="activeTab = 'settings'">SETTINGS</button>
              <button class="header-tab" v-if="nodeResult" @click="emit('clear-execution', node.id)" title="Clear execution data">CLEAR</button>
            </div>
            <div class="header-actions">
              <button class="btn-save" @click="emit('save')" title="Save workflow">
                <span class="btn-content">💾 Save</span>
              </button>
              <button class="btn-execute" :class="{ 'is-executing': executing }" @click="emit('execute', node.id, { single: true })">
                <span class="btn-content">⚡ Execute Step</span>
              </button>
            </div>
          </div>
          
          <div class="col-content">
            <div v-if="activeTab === 'parameters'" class="params-form">
              <!-- Webhook URL Display -->
              <div v-if="node.data.config.type === 'webhook'" class="form-row webhook-url-row">
                <label class="field-label" style="color: var(--teal)" title="Send a POST request with JSON data to this URL to trigger the workflow.">
                  Unique Webhook URL
                  <span class="info-icon" style="opacity: 0.5; font-size: 10px; cursor: help;">ⓘ</span>
                </label>
                <div class="webhook-url-input-group">
                  <input type="text" :value="webhookUrl" readonly />
                  <button @click="copyWebhookUrl" class="btn-copy-url" title="Copy URL">
                    <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M16 1H4C2.9 1 2 1.9 2 3v14h2V3h12V1zm3 4H8C6.9 5 6 5.9 6 7v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/></svg>
                  </button>
                </div>

              </div>

              <!-- Dynamic Props -->
              <template v-for="prop in nodeDefinition.properties" :key="prop.name">
                <div v-if="shouldShowProperty(prop)" class="form-row" :class="'row-' + prop.type">
                  <div v-if="prop.type === 'curlImport'" class="curl-import-row">
                    <button class="btn-curl-import" @click="openCurlImport">
                      <span class="btn-icon">📋</span>
                      Import cURL
                    </button>
                  </div>
                  <template v-else-if="prop.type === 'boolean'">
                    <label class="field-label">{{ prop.displayName }}</label>
                    <div class="toggle-field">
                      <label class="toggle-switch">
                        <input type="checkbox" v-model="node.data.config[prop.name]" />
                        <span class="toggle-track"><span class="toggle-thumb"></span></span>
                      </label>
                      <span class="toggle-label">{{ node.data.config[prop.name] ? 'Enabled' : 'Disabled' }}</span>
                    </div>
                  </template>
                  <template v-else-if="prop.type === 'options'">
                    <label>{{ prop.displayName }}</label>
                    <SearchableSelect 
                      v-if="prop.searchable || prop.allowCustomValue"
                      v-model="node.data.config[prop.name]"
                      :options="prop.options"
                      :allow-custom-value="!!prop.allowCustomValue"
                      :placeholder="prop.placeholder || (prop.allowCustomValue ? 'Select or type...' : 'Search...')"
                    />
                    <select v-else v-model="node.data.config[prop.name]">
                      <option v-for="opt in prop.options" :key="opt.value" :value="opt.value">{{ optionDisplayName(opt) }}</option>
                    </select>
                    
                  </template>
                  <template v-else-if="prop.type === 'multiOptions'">
                    <label>{{ prop.displayName }}</label>
                      <div class="multi-options-field">
                        <div class="mo-selected-tags">
                          <span v-for="(val, idx) in (node.data.config[prop.name] || [])" :key="val" class="mo-tag">
                          {{ getOptionLabel(prop, val) }}
                          <button class="mo-tag-remove" type="button" @click="node.data.config[prop.name].splice(idx, 1)">✕</button>
                        </span>
                      </div>
                      <SearchableSelect
                        :modelValue="''"
                        :options="(prop.options || []).filter(o => !(node.data.config[prop.name] || []).includes(o.value))"
                        :placeholder="prop.placeholder || `Search ${prop.displayName.toLowerCase()}...`"
                        @update:modelValue="(v) => { if (v) { if (!node.data.config[prop.name]) node.data.config[prop.name] = []; node.data.config[prop.name].push(v); } }"
                      />
                    </div>
                    
                  </template>
                  <template v-else-if="prop.type === 'credential'">
                    <label class="field-label">{{ prop.displayName }}</label>
                    <select v-model="node.data.config[prop.name]">
                      <option value="">-- None --</option>
                      <option v-for="cred in getCredentialsForService(prop.service)" :key="cred.id" :value="cred.id">{{ cred.name }}</option>
                    </select>
                  </template>
                  <template v-else-if="prop.typeOptions?.rows">
                    <label>{{ prop.displayName }}</label>
                    <div class="input-with-preview">
                      <textarea 
                        :rows="prop.typeOptions.rows" 
                        v-model="node.data.config[prop.name]" 
                        :class="{ 'has-expression': hasExpression(node.data.config[prop.name]), 'focused-exp': isFieldFocused({ name: prop.name }) }"
                        @drop.prevent="onDrop($event, prop.name)" 
                        @dragover.prevent
                        @focus="handleFocus($event, { name: prop.name })"
                        @blur="handleBlur"
                        placeholder="Drop variables here..."
                      ></textarea>
                      <button type="button" class="btn-expand-input" title="Expand to view full value"
                        @click="openExpandedInput(prop.displayName, node.data.config[prop.name], v => node.data.config[prop.name] = v)">
                        <svg viewBox="0 0 24 24" width="13" height="13"><path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
                      </button>

                      <!-- Bottom Dropdown Preview -->
                      <Transition name="fade">
                        <div 
                          v-if="isFieldFocused({ name: prop.name }) && hasExpression(node.data.config[prop.name])" 
                          class="nd-dropdown-preview"
                          @mousedown="keepPreview = true"
                          @mouseup="releasePreview"
                          @mouseleave="releasePreview"
                        >
                          <div class="fp-header"><span>RESULT</span></div>
                          <div class="fp-body">{{ focusedValue || (focusedValue === '' ? '(Empty String)' : '(Waiting for data...)') }}</div>
                        </div>
                      </Transition>

                      <!-- Persistent resolved value (n8n style) -->
                      <div
                        v-if="hasExpression(node.data.config[prop.name]) && !isFieldFocused({ name: prop.name })"
                        class="exp-resolved"
                      >
                        <span class="exp-resolved-icon">=</span>
                        <span class="exp-resolved-val">{{ inlineResolved(node.data.config[prop.name]) ?? '(run previous node to preview)' }}</span>
                      </div>
                    </div>
                  </template>
                  <template v-else-if="prop.type === 'fixedCollection'">
                    <div class="collection-wrapper">
                      <div class="collection-header">
                        <label>{{ prop.displayName }}</label>
                      </div>
                      <div class="fixed-collection">
                      <div class="fc-items">
                        <div v-for="(item, idx) in (node.data.config[prop.name]?.parameters || [])" :key="idx" class="fc-item" style="flex-direction: column;">
                          <!-- Full-width rows (e.g. textareas). Use <template> so
                               sub-fields without a row textarea don't leave empty
                               flex children behind — those were injecting dead
                               vertical space (the fc-item column has a gap) and
                               spreading the rules far apart. -->
                          <template v-for="subProp in prop.options" :key="'row-'+subProp.name">
                            <div v-if="shouldShowProperty(subProp, item) && subProp.typeOptions?.rows" class="fc-sub-field" style="width: 100%;">

                              <div class="input-with-preview">
                                <textarea
                                  v-model="item[subProp.name]"
                                  :rows="subProp.typeOptions.rows"
                                  :class="{ 'has-expression': hasExpression(item[subProp.name]), 'focused-exp': isFieldFocused({ collection: prop.name, index: idx, subName: subProp.name }) }"
                                  :placeholder="subProp.placeholder || ''"
                                  @drop.prevent="onDropCollection($event, prop.name, idx, subProp.name)"
                                  @dragover.prevent
                                  @focus="handleFocus($event, { collection: prop.name, index: idx, subName: subProp.name })"
                                  @blur="handleBlur"
                                ></textarea>
                                <button type="button" class="btn-expand-input" title="Expand to view full value"
                                  @click="openExpandedInput(subProp.displayName, item[subProp.name], v => item[subProp.name] = v)">
                                  <svg viewBox="0 0 24 24" width="13" height="13"><path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
                                </button>
                                <!-- Bottom Dropdown Preview -->
                                <Transition name="fade">
                                  <div 
                                    v-if="isFieldFocused({ collection: prop.name, index: idx, subName: subProp.name }) && hasExpression(item[subProp.name])" 
                                    class="nd-dropdown-preview"
                                    @mousedown="keepPreview = true"
                                    @mouseup="keepPreview = false"
                                    @mouseleave="keepPreview = false"
                                  >
                                    <div class="fp-header"><span>RESULT</span></div>
                                    <div class="fp-body">{{ focusedValue || (focusedValue === '' ? '(Empty String)' : '(Waiting for data...)') }}</div>
                                  </div>
                                </Transition>
                              </div>
                            </div>
                          </template>

                          <!-- Inline grouped fields -->
                          <div style="display: flex; gap: 8px; width: 100%; align-items: flex-start;">
                            <div class="fc-item-fields">
                            <template v-for="subProp in prop.options" :key="'inline-'+subProp.name">
                              <div v-if="shouldShowProperty(subProp, item) && !subProp.typeOptions?.rows" class="fc-sub-field" :class="{ 'fc-sub-nolabel': subProp.hideLabel }">
                                <label v-if="idx === 0 && !subProp.hideLabel" class="fc-header-label" style="display: block; margin-bottom: 4px;">{{ subProp.displayName }}</label>
                                <div class="input-with-preview">
                                  <template v-if="subProp.type === 'options'">
                                    <SearchableSelect
                                      v-if="subProp.searchable || subProp.allowCustomValue"
                                      v-model="item[subProp.name]"
                                      :options="filteredOptions(subProp, item)"
                                      :allow-custom-value="!!subProp.allowCustomValue"
                                      :placeholder="subProp.placeholder || (subProp.allowCustomValue ? 'Select or type...' : 'Search...')"
                                    />
                                    <select
                                      v-else
                                      v-model="item[subProp.name]"
                                      :class="{ 'has-expression': hasExpression(item[subProp.name]), 'focused-exp': isFieldFocused({ collection: prop.name, index: idx, subName: subProp.name }) }"
                                      @focus="handleFocus($event, { collection: prop.name, index: idx, subName: subProp.name })"
                                      @blur="handleBlur"
                                    >
                                      <option v-for="opt in filteredOptions(subProp, item)" :key="opt.value" :value="opt.value">{{ optionDisplayName(opt) }}</option>
                                    </select>
                                  </template>
                                  <template v-else-if="subProp.type === 'boolean'">
                                    <div class="fc-toggle">
                                      <label class="fc-toggle-switch" :title="subProp.displayName">
                                        <input type="checkbox" v-model="item[subProp.name]" />
                                        <span class="fc-toggle-slider"></span>
                                      </label>
                                    </div>
                                  </template>
                                  <input
                                    v-else
                                    :type="subProp.type === 'number' ? 'number' : 'text'"
                                    v-model="item[subProp.name]"
                                    :class="{ 'has-expression': hasExpression(item[subProp.name]), 'focused-exp': isFieldFocused({ collection: prop.name, index: idx, subName: subProp.name }) }"
                                    :placeholder="subProp.placeholder || ''"
                                    @drop.prevent="onDropCollection($event, prop.name, idx, subProp.name)"
                                    @dragover.prevent
                                    @focus="handleFocus($event, { collection: prop.name, index: idx, subName: subProp.name })"
                                    @blur="handleBlur"
                                  />
                                  <button v-if="subProp.type !== 'options' && subProp.type !== 'boolean'" type="button" class="btn-expand-input" title="Expand to view full value"
                                    @click="openExpandedInput(subProp.displayName, item[subProp.name], v => item[subProp.name] = v)">
                                    <svg viewBox="0 0 24 24" width="13" height="13"><path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
                                  </button>
                                  <!-- Bottom Dropdown Preview -->
                                  <Transition name="fade">
                                    <div 
                                      v-if="isFieldFocused({ collection: prop.name, index: idx, subName: subProp.name }) && hasExpression(item[subProp.name])" 
                                      class="nd-dropdown-preview"
                                      @mousedown="keepPreview = true"
                                      @mouseup="keepPreview = false"
                                      @mouseleave="keepPreview = false"
                                    >
                                      <div class="fp-header"><span>RESULT</span></div>
                                      <div class="fp-body">{{ focusedValue || (focusedValue === '' ? '(Empty String)' : '(Waiting for data...)') }}</div>
                                    </div>
                                  </Transition>
                                </div>
                              </div>
                            </template>
                          </div>
                          <button class="btn-fc-remove" type="button" @click="removeCollectionItem(prop.name, idx)" title="Remove item">✕</button>
                          </div> <!-- close the flex row wrapper -->
                        </div>
                      </div>
                      <button class="btn-fc-add" type="button" @click="addCollectionItem(prop.name, prop.options)">+ {{ prop.placeholder || 'Add Item' }}</button>
                    </div>
                  </div>
                </template>
                  <template v-else-if="prop.type === 'collection'">
                    <div class="collection-wrapper">
                      <div class="collection-header">
                        <label>{{ prop.displayName }}</label>
                      </div>
                      <div v-if="node.data.config[prop.name]" class="collection-field">
                      <div class="cf-items">
                        <div v-for="opt in prop.options" :key="opt.name" class="cf-item">
                          <template v-if="shouldShowProperty(opt, node.data.config[prop.name])">
                            <div v-if="opt.type === 'boolean'" class="cf-row-boolean">
                              <input type="checkbox" v-model="node.data.config[prop.name][opt.name]" :id="prop.name + opt.name" />
                              <label :for="prop.name + opt.name">{{ opt.displayName }}</label>
                            </div>
                            <div v-else-if="opt.type === 'string'" class="cf-row">
                              <label>{{ opt.displayName }}</label>
                              <input type="text" v-model="node.data.config[prop.name][opt.name]" :placeholder="opt.placeholder" />
                            </div>
                            <div v-else-if="opt.type === 'number'" class="cf-row">
                              <label>{{ opt.displayName }}</label>
                              <input type="number" v-model="node.data.config[prop.name][opt.name]" />
                            </div>
                          </template>
                        </div>
                      </div>
                    </div>
                  </div>
                </template>
                  <template v-else-if="prop.type === 'inlineGroup'">
                    <div class="collection-wrapper">
                      <div class="fixed-collection" style="padding-top: 8px;">
                        <div class="fc-items">
                          <div class="fc-item">
                            <div class="fc-item-fields">
                              <template v-for="subProp in prop.options" :key="subProp.name">
                                <div v-if="shouldShowProperty(subProp)" class="fc-sub-field" :class="{'field-full-row': subProp.typeOptions?.rows}">
                                  <label class="fc-header-label" style="display: block; margin-bottom: 4px;">{{ subProp.displayName }}</label>
                                  <div class="input-with-preview">
                                  <template v-if="subProp.type === 'options'">
                                    <SearchableSelect 
                                      v-if="subProp.searchable || subProp.allowCustomValue"
                                      v-model="node.data.config[subProp.name]"
                                      :options="subProp.options"
                                      :allow-custom-value="!!subProp.allowCustomValue"
                                      :placeholder="subProp.placeholder || (subProp.allowCustomValue ? 'Select or type...' : 'Search...')"
                                    />
                                    <select 
                                      v-else
                                      v-model="node.data.config[subProp.name]"
                                      :class="{ 'has-expression': hasExpression(node.data.config[subProp.name]), 'focused-exp': isFieldFocused({ name: subProp.name }) }"
                                      @focus="handleFocus($event, { name: subProp.name })"
                                      @blur="handleBlur"
                                    >
                                      <option v-for="opt in subProp.options" :key="opt.value" :value="opt.value">{{ optionDisplayName(opt) }}</option>
                                    </select>
                                  </template>
                                  <div v-else-if="subProp.type === 'dateTime'" class="datetime-field">
                                    <input
                                      type="datetime-local"
                                      :value="isoToLocal(node.data.config[subProp.name])"
                                      @change="e => node.data.config[subProp.name] = localToIso(e.target.value)"
                                    />
                                    <span v-if="node.data.config[subProp.name]" class="datetime-iso-hint">{{ node.data.config[subProp.name] }}</span>
                                  </div>
                                  <input
                                    v-else
                                    :type="subProp.type === 'number' ? 'number' : 'text'"
                                    v-model="node.data.config[subProp.name]"
                                    :class="{ 'has-expression': hasExpression(node.data.config[subProp.name]), 'focused-exp': isFieldFocused({ name: subProp.name }) }"
                                    :placeholder="subProp.placeholder || ''"
                                    @drop.prevent="onDrop($event, subProp.name)"
                                    @dragover.prevent
                                    @focus="handleFocus($event, { name: subProp.name })"
                                    @blur="handleBlur"
                                  />
                                  <button v-if="subProp.type !== 'options' && subProp.type !== 'dateTime'" type="button" class="btn-expand-input" title="Expand to view full value"
                                    @click="openExpandedInput(subProp.displayName, node.data.config[subProp.name], v => node.data.config[subProp.name] = v)">
                                    <svg viewBox="0 0 24 24" width="13" height="13"><path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
                                  </button>
                                    <Transition name="fade">
                                      <div 
                                        v-if="isFieldFocused({ name: subProp.name }) && hasExpression(node.data.config[subProp.name])" 
                                        class="nd-dropdown-preview"
                                        @mousedown="keepPreview = true"
                                        @mouseup="keepPreview = false"
                                        @mouseleave="keepPreview = false"
                                      >
                                        <div class="fp-header"><span>RESULT</span></div>
                                        <div class="fp-body">{{ focusedValue || (focusedValue === '' ? '(Empty String)' : '(Waiting for data...)') }}</div>
                                      </div>
                                    </Transition>
                                  </div>
                                </div>
                              </template>
                            </div>
                          </div>
                        </div>
                      </div>
                    </div>
                  </template>
                  <template v-else-if="prop.type === 'dateTime'">
                    <label>📅 {{ prop.displayName }}</label>
                    <div class="datetime-field">
                      <input
                        type="datetime-local"
                        :value="isoToLocal(node.data.config[prop.name])"
                        @change="e => node.data.config[prop.name] = localToIso(e.target.value)"
                      />
                      <span v-if="node.data.config[prop.name]" class="datetime-iso-hint">{{ node.data.config[prop.name] }}</span>
                    </div>
                  </template>
                  <template v-else-if="prop.type === 'string' || prop.type === 'number'">
                    <label>{{ prop.displayName }}</label>
                    <div class="input-with-preview">
                      <input
                        type="text"
                        v-model="node.data.config[prop.name]"
                        :class="{ 'has-expression': hasExpression(node.data.config[prop.name]), 'focused-exp': isFieldFocused({ name: prop.name }) }"
                        @drop.prevent="onDrop($event, prop.name)"
                        @dragover.prevent
                        @focus="handleFocus($event, { name: prop.name })"
                        @blur="handleBlur"
                        placeholder="Drop variables here..."
                      />
                      <button type="button" class="btn-expand-input" title="Expand to view full value"
                        @click="openExpandedInput(prop.displayName, node.data.config[prop.name], v => node.data.config[prop.name] = v)">
                        <svg viewBox="0 0 24 24" width="13" height="13"><path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
                      </button>

                      <!-- Bottom Dropdown Preview -->
                      <Transition name="fade">
                        <div 
                          v-if="isFieldFocused({ name: prop.name }) && hasExpression(node.data.config[prop.name])" 
                          class="nd-dropdown-preview"
                          @mousedown="keepPreview = true"
                          @mouseup="releasePreview"
                          @mouseleave="releasePreview"
                        >
                          <div class="fp-header"><span>RESULT</span></div>
                          <div class="fp-body">{{ focusedValue || (focusedValue === '' ? '(Empty String)' : '(Waiting for data...)') }}</div>
                        </div>
                      </Transition>

                      <!-- Persistent resolved value (n8n style) -->
                      <div
                        v-if="hasExpression(node.data.config[prop.name]) && !isFieldFocused({ name: prop.name })"
                        class="exp-resolved"
                      >
                        <span class="exp-resolved-icon">=</span>
                        <span class="exp-resolved-val">{{ inlineResolved(node.data.config[prop.name]) ?? '(run previous node to preview)' }}</span>
                      </div>
                    </div>
                  </template>
                </div>
              </template>
            </div>
            <div v-else class="settings-form">
              <div class="form-row row-boolean-field">
                <label class="field-label">Enabled</label>
                <div class="toggle-field">
                  <label class="toggle-switch">
                    <input type="checkbox" v-model="node.data.enabled" @change="onSettingsChange" />
                    <span class="toggle-track"><span class="toggle-thumb"></span></span>
                  </label>
                  <span class="toggle-label">{{ node.data.enabled !== false ? 'Node Enabled' : 'Node Disabled' }}</span>
                </div>
              </div>
              <div class="form-row row-boolean-field">
                <label class="field-label">Output</label>
                <div class="toggle-field">
                  <label class="toggle-switch">
                    <input type="checkbox" v-model="node.data.alwaysOutputData" @change="onSettingsChange" />
                    <span class="toggle-track"><span class="toggle-thumb"></span></span>
                  </label>
                  <span class="toggle-label">{{ node.data.alwaysOutputData ? 'Always Output Data' : 'Only on Success' }}</span>
                </div>
              </div>
              <div class="form-row row-boolean-field">
                <label class="field-label">On Fail</label>
                <div class="toggle-field">
                  <label class="toggle-switch">
                    <input type="checkbox" v-model="node.data.continueOnFail" @change="onSettingsChange" />
                    <span class="toggle-track"><span class="toggle-thumb"></span></span>
                  </label>
                  <span class="toggle-label">{{ node.data.continueOnFail ? 'Continue On Fail' : 'Stop On Fail' }}</span>
                </div>
              </div>
              <div class="form-row row-boolean-field" v-if="node.data && node.data.config">
                <label class="field-label">Loop Items</label>
                <div class="toggle-field">
                  <label class="toggle-switch">
                    <input type="checkbox" v-model="node.data.config.execute_once" @change="onSettingsChange" />
                    <span class="toggle-track"><span class="toggle-thumb"></span></span>
                  </label>
                  <span class="toggle-label">{{ node.data.config.execute_once ? 'Run Once (aggregate all items)' : 'Run Per Item' }}</span>
                </div>
              </div>
            </div>
          </div>

          <!-- Expanded input editor — overlays the PARAMETERS column only, so the
               INPUT column stays visible and you can still drag expressions from
               upstream nodes straight into this larger editor. -->
          <Transition name="fade">
            <div v-if="expandedInput" class="nd-expand-panel">
              <header class="nd-expand-head">
                <span class="nd-expand-title" :title="expandedInput.title">{{ expandedInput.title }}</span>
                <button class="nd-expand-close" @click="closeExpandedInput" title="Close (Esc)">✕</button>
              </header>
              <div class="nd-expand-body">
                <textarea
                  ref="expandedInputRef"
                  class="nd-expand-textarea"
                  :class="{ 'has-expression': hasExpression(expandedInput.value) }"
                  :value="expandedInput.value"
                  @input="updateExpandedInput"
                  @drop.prevent="onExpandedDrop"
                  @dragover.prevent
                  @keydown.esc="closeExpandedInput"
                  placeholder="Edit the full value here — drag fields from the INPUT panel to insert expressions."
                  spellcheck="false"
                ></textarea>
              </div>
              <footer class="nd-expand-foot">
                <button class="btn-import" @click="closeExpandedInput">Done</button>
              </footer>
            </div>
          </Transition>
        </section>

        <!-- Right Resizer -->
        <div class="nd-resizer" @mousedown="startResize('right', $event)">
          <div class="resizer-handle"></div>
        </div>

        <!-- COLUMN: OUTPUT -->
        <section class="nd-col nd-col-right" :style="{ width: panelWidths.right + 'px' }">
          <div class="col-header" :class="{ 'col-header-error': nodeResult?.error }">
            <span class="col-title">OUTPUT <span v-if="nodeResult?.error" class="error-badge">ERROR</span></span>
            <div class="col-toggles">
              <button :class="{ active: outputMode === 'schema' }" @click="outputMode = 'schema'">Schema</button>
              <button :class="{ active: outputMode === 'table' }" @click="outputMode = 'table'">Table</button>
              <button :class="{ active: outputMode === 'json' }" @click="outputMode = 'json'">JSON</button>
            </div>
          </div>
          <div class="col-content">
            <!-- Node Error View -->
            <div v-if="errorDisplay" class="node-error-view">
              <div class="error-view-header">
                <div class="error-view-msg">{{ errorDisplay.message || 'Execution Failed' }}</div>
              </div>
              <div class="error-view-body">
                <p v-if="errorDisplay.description" class="error-view-desc">{{ errorDisplay.description }}</p>
                
                <details class="error-details-expand">
                  <summary>View raw error details</summary>
                  <pre class="error-stack">{{ errorDisplay.stack || errorDisplay.message }}</pre>
                </details>
                
                <button class="btn-copy-error" @click="copyErrorDetails">
                  <svg viewBox="0 0 24 24" width="14" height="14" style="margin-right: 6px;"><path fill="currentColor" d="M16 1H4C2.9 1 2 1.9 2 3v14h2V3h12V1zm3 4H8C6.9 5 6 5.9 6 7v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/></svg>
                  Copy Error Details
                </button>
              </div>
            </div>

            <!-- Output Data View -->
            <div v-else-if="nodeResult?.output || nodeResult?.file" class="output-data">
              <div class="dn-head" style="display: flex; justify-content: space-between; width: 100%;">
                <span class="dn-meta success-meta">✓ Execution Successful</span>
                <button v-if="nodeResult?.output" class="btn-copy-output" @click="copyOutput" title="Copy Output to Clipboard">
                  <svg viewBox="0 0 24 24" width="14" height="14" style="margin-right: 4px;"><path fill="currentColor" d="M16 1H4C2.9 1 2 1.9 2 3v14h2V3h12V1zm3 4H8C6.9 5 6 5.9 6 7v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/></svg>
                  Copy
                </button>
              </div>
              <div class="dn-body">
                <!-- Binary File Download -->
                <div v-if="nodeResult?.file" class="binary-file-info">
                  <div class="file-card">
                    <div class="file-card-icon">
                      <svg viewBox="0 0 24 24" width="24" height="24"><path fill="currentColor" d="M14 2H6c-1.1 0-1.99.9-1.99 2L4 20c0 1.1.89 2 1.99 2H18c1.1 0 2-.9 2-2V8l-6-6zM6 20V4h7v5h5v11H6z"/></svg>
                    </div>
                    <div class="file-card-details">
                      <div class="file-card-name">{{ nodeResult.file.original_name }}</div>
                      <div class="file-card-meta">
                        {{ nodeResult.file.mime_type }} • {{ (nodeResult.file.size / 1024).toFixed(1) }} KB
                      </div>
                    </div>
                    <a :href="'/api/download?path=' + encodeURIComponent(nodeResult.file.local_path)" class="btn-download-action" download>
                      <svg viewBox="0 0 24 24" width="16" height="16" style="margin-right: 6px;"><path fill="currentColor" d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z"/></svg>
                      Download
                    </a>
                  </div>
                </div>

                <pre v-if="outputMode === 'json' && nodeResult?.output" class="data-json">{{ formatOutput(nodeResult.output) }}</pre>
                <div v-else-if="outputMode === 'table'" class="output-table">
                  <table>
                    <tr v-for="(item, idx) in tableData" :key="idx">
                      <td v-for="(v, k) in item" :key="k"><b>{{ k }}:</b> {{ v }}</td>
                    </tr>
                  </table>
                </div>
                <div v-else class="data-tree">
                  <div 
                    v-for="field in getSchema(nodeResult.output)" 
                    :key="field.fullPath" 
                    class="dt-row"
                    draggable="true"
                    @dragstart="onDragStart($event, node.data.label, field.fullPath)"
                   >
                    <span class="dt-type" :class="field.type">{{ field.type === 'string' ? 'T' : (field.type === 'number' ? '#' : (field.type === 'boolean' ? '✓' : '{}')) }}</span>
                    <span class="dt-key">{{ field.key }}</span>
                    <span class="dt-val">{{ field.value }}</span>
                  </div>
                </div>
              </div>
            </div>
            <div v-else class="data-empty">
              Execute this node to see output data
            </div>
          </div>
        </section>
      </div>
    </div>
      </div>

      <!-- cURL Import Modal -->
      <Teleport to="body">
        <div v-if="showCurlModal" class="curl-modal-overlay" @click.self="showCurlModal = false">
          <div class="curl-modal">
            <header>
              <h3>Import cURL</h3>
              <button @click="showCurlModal = false">✕</button>
            </header>
            <div class="modal-body">
              <p>Paste your cURL command below to automatically populate this node.</p>
              <textarea 
                v-model="curlInput" 
                placeholder="curl -X POST https://api.example.com -H 'Content-Type: application/json' -d '{&quot;key&quot;:&quot;value&quot;}'"
                rows="10"
              ></textarea>
            </div>
            <footer>
              <button class="btn-cancel" @click="showCurlModal = false">Cancel</button>
              <button class="btn-import" @click="importCurl" :disabled="!curlInput">Import</button>
            </footer>
          </div>
        </div>
      </Teleport>
    </div>
  </Teleport>
</template>

<style scoped>
/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   DESIGN TOKENS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
/* Colors (dark theme — matches global :root palette in style.css) */
/* bg:      #0a0a0c | surface: #16161a | card: #1b1b20 | border: rgba(255, 255, 255, 0.09) */
/* primary: #6366f1 | accent:  #2c9b8d | text: #f2f7ff | muted: #a6a6b2 */

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   OVERLAY & WINDOW
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-overlay {
  position: fixed; inset: 0;
  --nd-nav-space: 72px;
  background: rgba(0, 0, 0, 0.7);
  backdrop-filter: blur(6px);
  z-index: 12000;
  display: flex;
  align-items: flex-start;
  justify-content: center;
  padding: 56px calc(20px + var(--nd-nav-space)) 24px;
}

.nd-window {
  width: min(1720px, 100%);
  height: min(980px, calc(100vh - 80px));
  position: relative;
  display: flex; flex-direction: column;
}

.nd-window-inner {
  width: 100%; height: 100%;
  background: #16161a;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 14px;
  display: flex; flex-direction: column;
  overflow: hidden;
  box-shadow: 0 24px 80px rgba(0,0,0,0.9), 0 0 0 1px rgba(255, 255, 255, 0.04) inset;
}


/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   FLOATING NAV
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-floating-nav {
  position: absolute; top: 50%; transform: translateY(-50%);
  display: flex; flex-direction: column; gap: 10px; z-index: 1001;
  max-height: calc(100% - 32px);
  overflow-y: auto;
  padding: 6px 2px;
}
.nd-floating-nav.left { left: calc(-1 * var(--nd-nav-space)); }
.nd-floating-nav.right { right: calc(-1 * var(--nd-nav-space)); }
.nd-floating-nav::-webkit-scrollbar { width: 4px; }
.nd-floating-nav::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.18);
  border-radius: 999px;
}
.nav-tab {
  width: 44px; height: 44px;
  background: #1f1f25;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 50%;
  display: flex; align-items: center; justify-content: center;
  font-size: 20px; cursor: pointer;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow: 0 4px 16px rgba(0,0,0,0.6);
}
.nav-tab:hover {
  background: #2f2f38;
  border-color: rgba(99,102,241,0.5);
  transform: scale(1.1);
  box-shadow: 0 4px 20px rgba(99,102,241,0.3);
}
.nav-tab-icon {
  width: 24px;
  height: 24px;
  object-fit: contain;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   HEADER
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-header {
  height: 58px;
  background: #1b1b20;
  padding: 0 20px;
  display: flex; justify-content: space-between; align-items: center;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  flex-shrink: 0;
}

.nd-header-left { display: flex; align-items: center; gap: 10px; }
.breadcrumbs { display: flex; align-items: center; gap: 8px; font-size: 12px; color: #a6a6b2; font-weight: 500; }
.bc-sep { opacity: 0.3; font-size: 11px; }
.bc-item { white-space: nowrap; }
.nd-header-icon { font-size: 18px; line-height: 1; }

.bc-item {
  color: rgba(255, 255, 255, 0.4);
  font-weight: 500;
}

.nd-header-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  margin: 0 4px;
  font-size: 20px;
  max-width: 28px;
  overflow: hidden;
}

.bc-icon-img {
  width: 20px;
  height: 20px;
  object-fit: contain;
}

.nd-inline-rename { display: flex; align-items: center; }
.nd-header-title {
  font-size: 14px; font-weight: 600; color: #f2f7ff;
  white-space: nowrap; cursor: text;
  padding: 3px 8px; border-radius: 6px;
  transition: background 0.15s;
  display: flex; align-items: center; gap: 8px;
}
.nd-header-title:hover { background: rgba(255, 255, 255, 0.06); }
.nd-header-subtitle { font-size: 10px; font-weight: 500; color: #a6a6b2; text-transform: uppercase; letter-spacing: 0.07em; }

.rename-input {
  font-size: 14px; font-weight: 600; color: var(--text);
  background: #1f1f25;
  border: 1px solid #6366f1;
  border-radius: 6px; padding: 3px 8px; outline: none; width: 220px;
  box-shadow: 0 0 0 3px rgba(99,102,241,0.15);
}

.nd-header-right { display: flex; gap: 20px; align-items: center; }
.btn-close {
  background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.09);
  color: #a6a6b2; font-size: 16px; cursor: pointer;
  width: 30px; height: 30px; border-radius: 8px;
  display: flex; align-items: center; justify-content: center;
  transition: all 0.15s;
}
.btn-close:hover { background: rgba(248,81,73,0.15); border-color: rgba(248,81,73,0.4); color: #f85149; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   BODY & COLUMNS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-body { flex: 1; display: flex; overflow: hidden; min-height: 0; }

.nd-col { display: flex; flex-direction: column; overflow: hidden; }
.nd-col-left  { background: #0e0e11; }
.nd-col-mid   { background: #141417; position: relative; }
.nd-col-right { background: #0e0e11; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   RESIZER
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-resizer {
  width: 4px; background: rgba(255, 255, 255, 0.05);
  cursor: col-resize; transition: background 0.15s;
  z-index: 10; display: flex; justify-content: center; align-items: center;
}
.nd-resizer:hover, .nd-resizer:active { background: #6366f1; }
.resizer-handle { width: 2px; height: 24px; background: rgba(255, 255, 255, 0.15); border-radius: 2px; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   COLUMN HEADERS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.col-header {
  height: 48px; padding: 0 16px;
  display: flex; align-items: center; justify-content: space-between;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  background: #1b1b20; gap: 10px; flex-shrink: 0;
}
.col-header-error { background: rgba(239,68,68,0.08); border-bottom-color: rgba(239,68,68,0.2); }

.col-title { font-size: 10px; font-weight: 700; color: #a6a6b2; letter-spacing: 0.12em; text-transform: uppercase; display: flex; align-items: center; }
.error-badge { background: #ef4444; color: #fff; font-size: 9px; padding: 2px 5px; border-radius: 4px; margin-left: 7px; font-weight: 700; }

.col-toggles { display: flex; gap: 2px; background: rgba(255, 255, 255, 0.04); padding: 3px; border-radius: 7px; border: 1px solid rgba(255, 255, 255, 0.06); }
.col-toggles button {
  background: none; border: none; padding: 4px 11px;
  font-size: 11px; color: #a6a6b2; cursor: pointer;
  border-radius: 5px; font-weight: 600; transition: all 0.15s;
}
.col-toggles button:hover { color: #f2f7ff; }
.col-toggles button.active { background: #2a2a32; color: #f2f7ff; box-shadow: 0 1px 4px rgba(0,0,0,0.4); }

.col-content { flex: 1; overflow-y: auto; padding: 16px; }
.col-content::-webkit-scrollbar { width: 4px; }
.col-content::-webkit-scrollbar-track { background: transparent; }
.col-content::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.1); border-radius: 2px; }

.data-json {
  white-space: pre-wrap;
  word-wrap: break-word;
  font-family: 'Fira Code', monospace;
  font-size: 11px;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   TABS & EXECUTE BUTTON
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.tabs { display: flex; gap: 0; align-items: center; }
.tabs button {
  background: none; border: none; color: #a6a6b2;
  font-size: 11px; font-weight: 700; cursor: pointer;
  padding: 0 14px; height: 48px;
  border-bottom: 2px solid transparent;
  transition: all 0.2s; text-transform: uppercase; letter-spacing: 0.05em;
}
.tabs button:hover { color: #f2f7ff; background: rgba(255, 255, 255, 0.04); }
.tabs button.active { color: #6366f1; border-bottom-color: #6366f1; }

.btn-tab-action {
  background: none; border: none; color: #a6a6b2; font-size: 10px;
  font-weight: 600; cursor: pointer; padding: 3px 8px; border-radius: 4px;
  margin-left: 4px; transition: all 0.15s; text-transform: uppercase;
}
.btn-tab-action:hover { color: #f2f7ff; background: rgba(255, 255, 255, 0.05); }

.header-actions { display: flex; align-items: center; gap: 8px; }

.btn-save {
  height: 34px;
  padding: 0 14px;
  box-sizing: border-box;
  background: rgba(255, 255, 255, 0.05);
  color: #d7dbe8;
  border: 1px solid rgba(255, 255, 255, 0.14);
  border-radius: 8px;
  font-size: 12px; font-weight: 600;
  cursor: pointer; white-space: nowrap;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  display: flex; align-items: center; gap: 6px;
}
.btn-save:hover {
  background: rgba(255, 255, 255, 0.1);
  border-color: rgba(255, 255, 255, 0.24);
  color: #f2f7ff;
  transform: translateY(-1px);
}
.btn-save:active { transform: translateY(0); }

.btn-execute {
  height: 34px;
  padding: 0 16px;
  box-sizing: border-box;
  background: linear-gradient(135deg, #4f46e5 0%, #6366f1 100%);
  color: #fff;
  border: 1px solid rgba(99,102,241,0.4);
  border-radius: 8px;
  font-size: 12px; font-weight: 600;
  cursor: pointer; white-space: nowrap;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow: 0 2px 10px rgba(79,70,229,0.35);
  display: flex; align-items: center; gap: 6px;
}
.btn-execute:hover {
  background: linear-gradient(135deg, #4338ca 0%, #4f46e5 100%);
  box-shadow: 0 4px 18px rgba(79,70,229,0.55);
  transform: translateY(-1px);
}
.btn-execute:active { transform: translateY(0); }
.btn-execute.is-executing { opacity: 0.7; cursor: not-allowed; animation: pulse-btn 1.5s infinite; }
@keyframes pulse-btn { 0%,100% { box-shadow: 0 2px 10px rgba(79,70,229,0.35); } 50% { box-shadow: 0 4px 22px rgba(79,70,229,0.65); } }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   PARAMS FORM - GRID ROW SYSTEM
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.params-form, .settings-form {
  display: flex; flex-direction: column; gap: 4px;
}

.form-row {
  display: grid;
  grid-template-columns: 140px 1fr;
  gap: 6px;
  align-items: center;
  padding: 2px 0;
  border-radius: 7px;
  transition: background 0.15s;
}
/* Textarea/multiline rows: label on top, input stretches full width */
.form-row.row-string {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 2px;
}
.form-row:hover { background: rgba(255, 255, 255, 0.02); }

/* Pill label inside form-row */
.form-row > label,
.form-row > .field-label {
  font-size: 10px; font-weight: 700;
  color: #a6a6b2; text-transform: uppercase; letter-spacing: 0.07em;
  padding: 0 8px;
  /* Wrap long field names onto multiple lines instead of truncating with an
     ellipsis — the 120px label column was clipping names so they were
     unreadable. align-items:center on .form-row keeps the input centered. */
  white-space: normal;
  overflow-wrap: break-word;
  word-break: break-word;
  line-height: 1.25;
}
/* For stacked rows (textarea), let label sit flush left without side padding */
.form-row.row-string > label {
  white-space: normal;
  overflow: visible;
  text-overflow: unset;
  padding: 0 2px;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   INPUTS & SELECTS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.form-row input[type="text"],
.form-row input[type="number"],
.form-row input[type="datetime-local"],
.form-row select,
.form-row textarea {
  width: 100%;
  background: #1f1f25;
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 7px;
  color: #f2f7ff;
  padding: 7px 11px;
  font-size: 13px;
  font-family: inherit;
  transition: border-color 0.2s, box-shadow 0.2s, background 0.2s;
  outline: none;
  box-sizing: border-box;
}
.form-row input[type="text"]:focus,
.form-row input[type="number"]:focus,
.form-row input[type="datetime-local"]:focus,
.form-row select:focus,
.form-row textarea:focus {
  border-color: #6366f1;
  background: #26262e;
  box-shadow: 0 0 0 3px rgba(99,102,241,0.15);
}
.form-row select { cursor: pointer; }
.form-row textarea { resize: vertical; min-height: 80px; line-height: 1.5; }

/* Webhook URL Field */
.webhook-url-row {
  background: rgba(129, 230, 217, 0.03);
  border: 1px solid rgba(129, 230, 217, 0.1);
  padding: 14px;
  border-radius: 12px;
  grid-column: 1 / -1;
  margin-bottom: 20px;
}
.webhook-url-input-group {
  display: flex;
  gap: 8px;
  margin-top: 8px;
}
.webhook-url-input-group input {
  flex: 1;
  background: #15151a !important;
  border: 1px solid rgba(129, 230, 217, 0.2) !important;
  border-radius: 8px !important;
  padding: 8px 12px !important;
  color: #56e6c8 !important;
  font-family: monospace !important;
  font-size: 11px !important;
  outline: none;
}
.btn-copy-url {
  background: rgba(129, 230, 217, 0.1);
  border: 1px solid rgba(129, 230, 217, 0.3);
  color: #2c9b8d;
  width: 34px;
  height: 34px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  cursor: pointer;
  transition: all 0.2s;
  flex-shrink: 0;
}
.btn-copy-url:hover {
  background: rgba(129, 230, 217, 0.2);
  border-color: #2c9b8d;
  transform: translateY(-1px);
}
.fc-toggle {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 32px;
}
.fc-toggle-switch {
  position: relative;
  display: inline-block;
  width: 32px;
  height: 18px;
  cursor: pointer;
}
.fc-toggle-switch input {
  opacity: 0;
  width: 0;
  height: 0;
}
.fc-toggle-slider {
  position: absolute;
  top: 0; left: 0; right: 0; bottom: 0;
  background: rgba(255, 255, 255, 0.12);
  border-radius: 20px;
  transition: all 0.2s;
  border: 1px solid rgba(255, 255, 255, 0.1);
}
.fc-toggle-slider::before {
  content: '';
  position: absolute;
  width: 12px;
  height: 12px;
  left: 3px;
  bottom: 2px;
  background: #a6a6b2;
  border-radius: 50%;
  transition: all 0.2s;
}
.fc-toggle-switch input:checked + .fc-toggle-slider {
  background: rgba(129, 230, 217, 0.2);
  border-color: rgba(129, 230, 217, 0.4);
}
.fc-toggle-switch input:checked + .fc-toggle-slider::before {
  transform: translateX(14px);
  background: #2c9b8d;
}

/* Description hint */
.form-row small,
small.form-desc {
  display: block;
  grid-column: 2;
  font-size: 11px;
  color: #a6a6b2;
  margin-top: 2px;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   BOOLEAN TOGGLE SWITCH
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.row-boolean-field { align-items: center; display: flex; flex-wrap: wrap; gap: 4px 10px; }
.row-boolean-field .toggle-field { display: flex; align-items: center; gap: 8px; }
.row-boolean-field > small {
  font-size: 11px;
  color: #a6a6b2;
  flex: 1 1 calc(100% - 46px);
}

.toggle-field {
  display: flex; align-items: center; gap: 10px;
}

.toggle-switch {
  position: relative;
  width: 36px; height: 20px;
  cursor: pointer; flex-shrink: 0;
}
.toggle-switch input { position: absolute; opacity: 0; width: 0; height: 0; }

.toggle-track {
  position: absolute; inset: 0;
  background: rgba(255, 255, 255, 0.1);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 20px;
  transition: background 0.25s, border-color 0.25s;
}
.toggle-switch input:checked ~ .toggle-track {
  background: rgba(99,102,241,0.7);
  border-color: #6366f1;
}
.toggle-thumb {
  position: absolute;
  top: 2px; left: 2px;
  width: 14px; height: 14px;
  background: #fff;
  border-radius: 50%;
  transition: transform 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow: 0 1px 4px rgba(0,0,0,0.4);
}
.toggle-switch input:checked ~ .toggle-track .toggle-thumb {
  transform: translateX(16px);
}

.toggle-label {
  font-size: 12px; font-weight: 500; color: #a6a6b2;
  transition: color 0.2s;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   cURL IMPORT BUTTON
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.curl-import-row { grid-column: 1 / -1; padding: 4px 0 8px; }

.btn-curl-import {
  display: inline-flex; align-items: center; gap: 8px;
  background: rgba(99,102,241,0.08);
  border: 1px solid rgba(99,102,241,0.25);
  color: #818cf8;
  padding: 7px 14px; border-radius: 8px;
  font-size: 12px; font-weight: 600;
  cursor: pointer; transition: all 0.2s;
}
.btn-curl-import:hover {
  background: rgba(99,102,241,0.18);
  border-color: rgba(99,102,241,0.5);
  color: #a5b4fc;
  box-shadow: 0 2px 12px rgba(99,102,241,0.2);
  transform: translateY(-1px);
}
.btn-icon { font-size: 14px; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   COLLECTIONS (fixedCollection / collection)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.collection-wrapper { grid-column: 1 / -1; margin: 8px 0; }

.collection-header { margin-bottom: 8px; display: flex; align-items: center; gap: 8px; }
.collection-header label {
  font-size: 10px; font-weight: 800; color: #6366f1;
  text-transform: uppercase; letter-spacing: 0.1em;
}

.fixed-collection, .collection-field {
  background: rgba(255, 255, 255, 0.03);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  padding: 12px;
  backdrop-filter: blur(2px);
}

/* FC header row */
.fc-header-row {
  display: flex; gap: 8px;
  padding-bottom: 8px; margin-bottom: 10px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.fc-header-label {
  flex: 1; font-size: 9px; font-weight: 800;
  color: #a6a6b2; text-transform: uppercase; letter-spacing: 0.08em;
}
.fc-header-spacer { width: 30px; }

/* FC items */
.fc-items { display: flex; flex-direction: column; gap: 6px; }
.fc-item { display: flex; gap: 8px; align-items: flex-start; }
.fc-item-fields { flex: 1; display: flex; gap: 8px; }
.fc-sub-field { flex: 1; display: flex; flex-direction: column; }
/* A label-less field (e.g. Switch's "Output Name") sits beside fields that DO
   carry a header label on the first row. Without this it floats up and lines up
   with their labels instead of their inputs. Anchoring its input to the bottom of
   the stretched row keeps it on the same baseline when everything fits on one
   line, and is a harmless no-op once the field wraps onto its own row. */
.fc-sub-field.fc-sub-nolabel { justify-content: flex-end; }

.fc-sub-field input[type="text"],
.fc-sub-field input[type="number"],
.fc-sub-field select {
  width: 100%; background: #15151a;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px; color: #f2f7ff;
  padding: 6px 10px; font-size: 12px;
  outline: none; transition: border-color 0.2s, box-shadow 0.2s; box-sizing: border-box;
}
.fc-sub-field input[type="text"]:focus,
.fc-sub-field input[type="number"]:focus,
.fc-sub-field select:focus {
  border-color: #6366f1;
  box-shadow: 0 0 0 3px rgba(99,102,241,0.15);
}

/* Collection field items */
.cf-items { display: flex; flex-direction: column; gap: 6px; }
.cf-row {
  display: grid; grid-template-columns: 120px 1fr; gap: 8px; align-items: center;
}
.cf-row label { font-size: 10px; font-weight: 700; color: #a6a6b2; text-transform: uppercase; letter-spacing: 0.07em; }
.cf-row input[type="text"], .cf-row input[type="number"] {
  background: #15151a; border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px; color: #f2f7ff; padding: 5px 9px; font-size: 12px;
  outline: none; transition: border-color 0.2s; width: 100%; box-sizing: border-box;
}
.cf-row input:focus { border-color: #6366f1; box-shadow: 0 0 0 3px rgba(99,102,241,0.12); }

.cf-row-boolean {
  display: flex; align-items: center; gap: 10px;
  padding: 6px 8px; border-radius: 6px; cursor: pointer;
  transition: background 0.15s;
}
.cf-row-boolean:hover { background: rgba(255, 255, 255, 0.03); }
.cf-row-boolean input[type="checkbox"] { width: 14px; height: 14px; accent-color: #6366f1; cursor: pointer; }
.cf-row-boolean label { font-size: 12px; color: #f2f7ff; cursor: pointer; font-weight: 500; }

/* Buttons inside collections */
.btn-fc-add {
  display: inline-flex; align-items: center; gap: 6px;
  background: rgba(99,102,241,0.1);
  border: 1px solid rgba(99,102,241,0.3);
  color: #818cf8;
  padding: 5px 12px; border-radius: 6px;
  font-size: 11px; font-weight: 600; cursor: pointer;
  margin-top: 8px; transition: all 0.2s;
}
.btn-fc-add:hover {
  background: rgba(99,102,241,0.22);
  border-color: rgba(99,102,241,0.6);
  color: #a5b4fc;
}

.btn-fc-remove {
  background: rgba(248,81,73,0.07);
  border: 1px solid rgba(248,81,73,0.2);
  color: #f85149;
  width: 28px; height: 28px;
  display: flex; align-items: center; justify-content: center;
  border-radius: 6px; cursor: pointer; flex-shrink: 0;
  font-size: 13px; transition: all 0.15s;
}
.btn-fc-remove:hover { background: rgba(248,81,73,0.2); border-color: rgba(248,81,73,0.5); }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   INPUT + EXPRESSION PREVIEW
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.input-with-preview { position: relative; }
.input-with-preview:focus-within { z-index: 1000; }

.datetime-field { display: flex; flex-direction: column; gap: 4px; }
.datetime-field input[type="datetime-local"] {
  width: 100%;
  color-scheme: dark;
}
.datetime-iso-hint {
  font-size: 11px;
  color: rgba(99,102,241,0.8);
  font-family: 'JetBrains Mono', monospace;
  padding-left: 2px;
  letter-spacing: 0.02em;
}

.form-row:focus-within,
.fc-item:focus-within,
.cf-row:focus-within {
  position: relative;
  z-index: 100;
}

.has-expression {
  background: rgba(99,102,241,0.06) !important;
  border-color: rgba(99,102,241,0.4) !important;
  color: #a5b4fc !important;
}
.focused-exp {
  border-color: #6366f1 !important;
  border-bottom-left-radius: 0 !important;
  border-bottom-right-radius: 0 !important;
}

.nd-dropdown-preview {
  position: absolute; top: 100%; left: 0; width: 100%;
  background: #1b1b20;
  border: 1px solid #6366f1;
  border-top: none;
  border-radius: 0 0 8px 8px;
  box-shadow: 0 10px 30px rgba(0,0,0,0.5);
  z-index: 1000; overflow: hidden;
}
.fp-header {
  padding: 4px 10px;
  background: rgba(99,102,241,0.12);
  border-bottom: 1px solid rgba(99,102,241,0.2);
}
.fp-header span { font-size: 9px; font-weight: 800; color: #818cf8; letter-spacing: 0.12em; }
.fp-body {
  padding: 8px 12px;
  font-family: 'Fira Code', monospace;
  font-size: 11px; color: #f2f7ff;
  background: #15151a; max-height: 160px; overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-all;
}

.fade-enter-active, .fade-leave-active { transition: opacity 0.18s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

/* Persistent resolved-value preview shown under expression fields (n8n style) */
.exp-resolved {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 4px;
  padding: 1px 2px;
  max-width: 100%;
  font-family: 'Fira Code', 'JetBrains Mono', monospace;
  font-size: 10.5px;
  line-height: 1.4;
  color: #7dd3a8;
  overflow: hidden;
}
.exp-resolved-icon { color: #818cf8; font-weight: 800; flex: 0 0 auto; }
.exp-resolved-val {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  opacity: 0.9;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   PARAMETERS PANEL (N8N STYLE PARITY)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */

.params-header {
  align-items: center;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  padding: 0 16px;
  height: 52px;
}

.header-tabs {
  display: flex;
  height: 100%;
  gap: 20px;
}

.header-tab {
  background: transparent;
  border: none;
  color: #a6a6b2;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.5px;
  cursor: pointer;
  position: relative;
  height: 100%;
  display: flex;
  align-items: center;
  transition: color 0.2s;
}

.header-tab:hover {
  color: #f2f7ff;
}

.header-tab.active {
  color: #818cf8;
}

.header-tab.active::after {
  content: '';
  position: absolute;
  bottom: 0;
  left: 0;
  right: 0;
  height: 2px;
  background: #6366f1;
}

.params-form, .settings-form {
  padding: 2px 2px;
}

.form-row {
  display: grid;
  grid-template-columns: 120px 1fr;
  align-items: center;
  gap: 4px;
  padding: 1px 4px;
  border-radius: 6px;
  transition: background 0.2s;
}
.form-row:hover {
  background: rgba(255, 255, 255, 0.02);
}

.field-label, .form-label, .form-row label {
  font-size: 11px;
  color: #a6a6b2;
  font-weight: 600;
  letter-spacing: 0.5px;
  text-transform: uppercase;
  margin: 0;
}
.required { color: #f85149; margin-left: 4px; }

/* Boolean toggle rows override grid */
.row-boolean-field {
  align-items: center;
}
.toggle-field, .toggle-row, .cf-row-boolean {
  display: flex;
  align-items: center;
  gap: 12px;
}
.toggle-switch, .toggle {
  position: relative;
  display: inline-flex;
  cursor: pointer;
}
.toggle-switch input, .toggle input {
  opacity: 0; width: 0; height: 0;
}
.toggle-track {
  width: 36px; height: 20px;
  background: rgba(255, 255, 255, 0.1);
  border-radius: 10px;
  transition: all 0.3s ease;
  position: relative;
}
.toggle-switch input:checked + .toggle-track, .toggle input:checked + .toggle-track {
  background: #6366f1;
}
.toggle-thumb {
  position: absolute;
  top: 2px; left: 2px;
  width: 16px; height: 16px;
  background: #fff;
  border-radius: 50%;
  transition: all 0.3s ease;
}
.toggle-switch input:checked + .toggle-track .toggle-thumb, .toggle input:checked + .toggle-track .toggle-thumb {
  transform: translateX(16px);
}
.toggle-label {
  font-size: 13px; color: #a6a6b2; font-weight: 500;
}

.input-with-preview {
  position: relative;
  width: 100%;
}
.form-input, .input-with-preview input, .input-with-preview textarea {
  width: 100%;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 6px 10px;
  border-radius: 6px;
  font-family: inherit;
  font-size: 13px;
  outline: none;
  resize: vertical;
  transition: all 0.2s;
}
.form-input:focus, .input-with-preview input:focus, .input-with-preview textarea:focus {
  background: rgba(255, 255, 255, 0.06);
  border-color: #6366f1;
}

/* Expand affordance — a small button that surfaces on hover/focus at the top
   corner of any text field, opening the full value in a roomy overlay editor.
   Kept out of the input's flow (absolute) so it never reshapes the field, and
   hidden until hover so it doesn't clutter the form. */
.btn-expand-input {
  position: absolute;
  top: 3px;
  right: 3px;
  width: 22px;
  height: 22px;
  padding: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(27, 27, 32, 0.9);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 5px;
  color: #a6a6b2;
  cursor: pointer;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.15s, color 0.15s, border-color 0.15s, background 0.15s;
  z-index: 6;
}
.input-with-preview:hover > .btn-expand-input,
.input-with-preview:focus-within > .btn-expand-input {
  opacity: 1;
  pointer-events: auto;
}
.btn-expand-input:hover {
  color: #a5b4fc;
  border-color: rgba(99, 102, 241, 0.5);
  background: rgba(40, 40, 55, 0.95);
}

.form-select, select {
  width: 100%;
  background: rgba(255, 255, 255, 0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 6px 10px;
  border-radius: 6px;
  font-size: 13px;
  appearance: none;
  outline: none;
}
.form-select:focus, select:focus { border-color: #6366f1; }

.curl-import-row {
  grid-column: 1 / -1;
  display: flex; justify-content: flex-start;
}
.btn-curl-import {
  display: inline-flex; align-items: center; gap: 6px;
  background: rgba(99,102,241,0.1);
  color: #818cf8;
  border: 1px solid rgba(99,102,241,0.3);
  padding: 6px 14px;
  border-radius: 6px;
  font-size: 12px; font-weight: 600; cursor: pointer;
  transition: all 0.2s;
}
.btn-curl-import:hover {
  background: rgba(99,102,241,0.2);
  color: #a5b4fc;
}

/* Collections */
.collection-wrapper {
  grid-column: 1 / -1;
  background: rgba(255, 255, 255, 0.02);
  border: 1px solid rgba(255, 255, 255, 0.06);
  border-radius: 8px;
  padding: 6px 10px;
}
.collection-header { margin-bottom: 6px; }
.fc-header-row {
  display: flex; gap: 8px; margin-bottom: 4px; padding-bottom: 4px; border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}
.fc-header-label {
  flex: 1; font-size: 11px; color: #a6a6b2; text-transform: uppercase; font-weight: 600;
}
.fc-header-spacer { width: 32px; } /* for the X button */
.fc-item {
  display: flex; gap: 6px; align-items: start; margin-bottom: 4px;
}
.fc-item-fields {
  display: flex; flex-wrap: wrap; gap: 6px; flex: 1;
}
.fc-sub-field { flex: 1; min-width: 90px; }
.fc-sub-field.field-full-row { flex-basis: 100%; width: 100%; }
.fc-sub-field input,
.fc-sub-field textarea,
.fc-sub-field select {
  width: 100%;
  background: rgba(255,255,255,0.04);
  border: 1px solid rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
  padding: 6px 8px;
  border-radius: 6px;
  font-size: 13px;
  outline: none;
  transition: border-color 0.2s;
}
.fc-sub-field textarea {
  resize: vertical;
  min-height: 52px;
}
.fc-sub-field input:focus,
.fc-sub-field textarea:focus,
.fc-sub-field select:focus { border-color: #6366f1; }
.btn-fc-remove {
  width: 32px; height: 32px; background: rgba(248,81,73,0.1); border: 1px solid rgba(248,81,73,0.2); color: #f85149;
  border-radius: 6px; display: flex; align-items: center; justify-content: center; cursor: pointer; transition: all 0.2s;
  flex-shrink: 0;
}
.btn-fc-remove:hover { background: rgba(248,81,73,0.2); border-color: rgba(248,81,73,0.5); }
.btn-fc-add {
  display: inline-block; background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.1); color: #f2f7ff;
  padding: 5px 10px; border-radius: 6px; font-size: 11px; font-weight: 600; cursor: pointer; transition: all 0.2s; margin-top: 6px;
}
.btn-fc-add:hover { background: rgba(255, 255, 255, 0.08); color: var(--text); border-color: rgba(255, 255, 255, 0.2); }

/* Regular collection string fields */
.cf-row {
  display: grid; grid-template-columns: 140px 1fr; align-items: center; gap: 12px; margin-bottom: 2px;
}
.cf-row label { font-size: 11px; color: #a6a6b2; text-transform: uppercase; font-weight: 600; }
.cf-row input {
  width: 100%; background: rgba(255, 255, 255, 0.04); border: 1px solid rgba(255, 255, 255, 0.1); color: #f2f7ff; padding: 6px 10px; border-radius: 6px; outline: none; font-size: 13px;
}
.cf-row input:focus { border-color: #6366f1; }
.cf-row-boolean {
  display: flex; align-items: center; gap: 10px; margin-bottom: 4px;
}
.cf-row-boolean input { accent-color: #6366f1; width: 14px; height: 14px; cursor: pointer; }
.cf-row-boolean label { font-size: 13px; color: #f2f7ff; cursor: pointer; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   INPUT PANEL (n8n Style)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.nd-input-layout {
  display: flex !important;
  flex-direction: row !important;
  padding: 0 !important;
  background: rgba(0, 0, 0, 0.25);
}

/* First-node empty state: .nd-input-layout zeroes its own padding, so the
   "No input data…" message would otherwise sit flush against the edge. Give
   it breathing room and let it span the full column. */
.nd-input-layout > .data-empty {
  width: 100%;
  box-sizing: border-box;
  padding: 16px;
  color: #a6a6b2;
  font-size: 12px;
  line-height: 1.5;
}

/* Removed gutter CSS */

.nd-input-main {
  flex-grow: 1;
  overflow-y: auto;
  padding: 12px 0;
}

.data-node {
  border: none;
  background: transparent;
  margin-bottom: 0px;
}

.dn-head {
  display: flex;
  align-items: center;
  padding: 10px 16px;
  gap: 12px;
  height: 32px;
  margin-bottom: 1px;
}

.dn-head:hover {
  background: rgba(255, 255, 255, 0.03);
}

.dn-chevron {
  width: 16px;
  height: 16px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(255, 255, 255, 0.35);
  transition: transform 0.2s;
  transform: rotate(-90deg);
}

.dn-chevron.active {
  transform: rotate(0);
}

.dn-label {
  font-size: 11px;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.85);
  white-space: nowrap;
}

.dn-meta {
  margin-left: auto;
  font-size: 9px;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  color: rgba(255, 255, 255, 0.3);
  font-weight: 700;
}

.dn-body {
  padding: 8px 16px 16px 28px;
}

/* data tree shared */
.data-tree {
  font-family: 'JetBrains Mono', 'Fira Code', monospace;
}

.dt-row {
  display: flex;
  align-items: center;
  height: 24px;
  gap: 8px;
  border-radius: 4px;
  padding-right: 8px;
  cursor: grab;
  transition: background 0.1s;
}

.dt-row:hover {
  background: rgba(99, 102, 241, 0.15);
}

.dt-row:active {
  cursor: grabbing;
}

.dt-type {
  font-size: 9px;
  padding: 1px 4px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.06);
  color: rgba(255, 255, 255, 0.5);
  width: 14px;
  text-align: center;
}

.dt-key {
  font-size: 11px;
  color: rgba(255, 255, 255, 0.75);
  font-weight: 500;
}

.dt-val {
  font-size: 11px;
  color: rgba(255, 255, 255, 0.4);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* JSON mode details */
.mode-json .dt-key { color: #f28c28; } /* Orange key */
.mode-json .dt-sep { color: rgba(255, 255, 255, 0.4); font-size: 11px; margin-left: -5px; }
.mode-json .dt-val { color: #2c9b8d; } /* Teal string */

.dt-type.number { color: #6366f1; }
.dt-type.boolean { color: #50fa7b; }
.dt-type.object { color: #feca57; }

.data-json {
  font-family: 'Fira Code', monospace; font-size: 12px;
  padding: 12px; background: transparent;
  color: #94a3b8;
  margin: 0; white-space: pre-wrap;
  border: none;
  max-width: 100%; overflow-x: auto; line-height: 1.6;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   OUTPUT PANEL (RIGHT)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */

.nd-col-right .col-content {
  padding: 4px;
}

.output-data .dn-head {
  padding: 8px 12px;
  background: rgba(255, 255, 255, 0.02);
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
}

.output-data .dn-body {
  padding: 0;
}

.output-table table { width: 100%; border-collapse: collapse; font-size: 12px; }
.output-table td { padding: 6px 8px; border-bottom: 1px solid rgba(255, 255, 255, 0.06); color: #94a3b8; word-break: break-all; }
.output-table td b { color: #818cf8; margin-right: 4px; }

/* Binary file card */
.binary-file-info { margin-bottom: 12px; }
.file-card {
  display: flex; align-items: center; gap: 12px;
  background: #1f1f25; border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px; padding: 12px 16px;
}
.file-card-icon { color: #a6a6b2; flex-shrink: 0; }
.file-card-details { flex: 1; min-width: 0; }
.file-card-name { font-size: 13px; font-weight: 600; color: #f2f7ff; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.file-card-meta { font-size: 11px; color: #a6a6b2; margin-top: 2px; }
.btn-download-action {
  display: flex; align-items: center; gap: 6px;
  background: rgba(99,102,241,0.1); border: 1px solid rgba(99,102,241,0.3);
  color: #818cf8; padding: 7px 14px; border-radius: 7px;
  font-size: 12px; font-weight: 600; text-decoration: none;
  transition: all 0.2s; white-space: nowrap; flex-shrink: 0;
}
.btn-download-action:hover { background: rgba(99,102,241,0.22); border-color: rgba(99,102,241,0.55); color: #a5b4fc; }

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   ERROR VIEW
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.node-error-view {
  background: rgba(248,81,73,0.05);
  border: 1px solid rgba(248,81,73,0.2);
  border-radius: 10px; margin-bottom: 12px; overflow: hidden;
}
.error-view-header {
  padding: 12px 16px;
  border-bottom: 1px solid rgba(248,81,73,0.12);
  background: rgba(248,81,73,0.04);
}
.error-view-msg { font-size: 13px; font-weight: 600; color: #f87171; }
.error-view-body { padding: 14px 16px; }
.error-view-desc { font-size: 13px; color: #a6a6b2; margin-bottom: 12px; line-height: 1.6; }
.error-details-expand summary {
  font-size: 11px; color: #a6a6b2; cursor: pointer;
  margin-bottom: 8px; font-weight: 600; letter-spacing: 0.04em;
}
.error-stack {
  background: #0a0a0c; padding: 12px; border-radius: 6px;
  font-family: 'Fira Code', monospace; font-size: 11px;
  color: #a6a6b2; overflow-x: auto; line-height: 1.5;
  border: 1px solid rgba(255, 255, 255, 0.06);
}
.btn-copy-error {
  display: inline-flex; align-items: center;
  background: rgba(255, 255, 255, 0.04); border: 1px solid rgba(255, 255, 255, 0.09);
  color: #a6a6b2; padding: 6px 12px; border-radius: 6px;
  font-size: 12px; font-weight: 500; cursor: pointer; margin-top: 12px;
  transition: all 0.15s;
}
.btn-copy-error:hover { background: rgba(255, 255, 255, 0.08); color: #f2f7ff; }

.btn-copy-output {
  display: flex;
  align-items: center;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.09);
  color: #94a3b8;
  padding: 4px 10px;
  border-radius: 4px;
  font-size: 11px;
  cursor: pointer;
  transition: all 0.2s;
}
.btn-copy-output:hover {
  background: rgba(255, 255, 255, 0.1);
  color: #f2f7ff;
}

/* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   cURL MODAL
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ */
.curl-modal-overlay {
  position: fixed; inset: 0;
  background: rgba(0,0,0,0.75);
  display: flex; align-items: center; justify-content: center;
  /* Must sit ABOVE .nd-overlay (z-index 12000) — both are teleported to <body>
     as siblings, so a lower value renders the modal behind the panel. */
  z-index: 12500; backdrop-filter: blur(6px);
}

/* Expanded input editor — overlays the PARAMETERS column only (absolute inset),
   so the INPUT column on the left stays visible and its fields remain draggable
   into the larger editor. Cleanly fills the column instead of floating over the
   whole screen. */
.nd-expand-panel {
  position: absolute;
  inset: 0;
  z-index: 2000;
  display: flex;
  flex-direction: column;
  background: #141417;
  border-left: 2px solid rgba(99, 102, 241, 0.5);
}
.nd-expand-head {
  display: flex; align-items: center; justify-content: space-between;
  gap: 12px;
  padding: 0 16px;
  height: 52px;
  flex-shrink: 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  background: #16161a;
}
.nd-expand-title {
  font-size: 13px; font-weight: 700; color: #f2f7ff;
  letter-spacing: 0.3px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.nd-expand-close {
  background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.09);
  color: #a6a6b2; font-size: 15px; cursor: pointer;
  width: 28px; height: 28px; border-radius: 6px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: center; transition: all 0.15s;
}
.nd-expand-close:hover { color: #f85149; border-color: rgba(248,81,73,0.4); }
.nd-expand-body {
  flex: 1;
  min-height: 0;
  display: flex;
  padding: 12px;
}
.nd-expand-textarea {
  width: 100%;
  flex: 1;
  background: #0a0a0c;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px; color: #f2f7ff;
  padding: 12px; font-family: 'Fira Code', monospace;
  font-size: 12.5px; resize: none; outline: none;
  transition: border-color 0.2s, box-shadow 0.2s; box-sizing: border-box;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  overflow-wrap: anywhere;
}
.nd-expand-textarea:focus { border-color: #6366f1; box-shadow: 0 0 0 3px rgba(99,102,241,0.15); }
.nd-expand-textarea.has-expression {
  border-color: rgba(99,102,241,0.45);
}
.nd-expand-foot {
  padding: 12px 16px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
  display: flex; justify-content: flex-end; gap: 10px;
  flex-shrink: 0;
  background: #16161a;
}
.curl-modal {
  width: 600px;
  background: #161619;
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 14px;
  box-shadow: 0 24px 80px rgba(0,0,0,0.8);
  overflow: hidden;
}
.curl-modal header {
  padding: 18px 20px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  display: flex; justify-content: space-between; align-items: center;
}
.curl-modal header h3 { font-size: 14px; font-weight: 700; color: #f2f7ff; margin: 0; }
.curl-modal header button {
  background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.09);
  color: #a6a6b2; font-size: 16px; cursor: pointer;
  width: 28px; height: 28px; border-radius: 6px;
  display: flex; align-items: center; justify-content: center; transition: all 0.15s;
}
.curl-modal header button:hover { color: #f85149; border-color: rgba(248,81,73,0.4); }
.curl-modal .modal-body { padding: 20px; }
.curl-modal .modal-body p { margin-bottom: 12px; color: #a6a6b2; font-size: 13px; line-height: 1.6; }
.curl-modal textarea {
  width: 100%; background: #0a0a0c;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 8px; color: #f2f7ff;
  padding: 12px; font-family: 'Fira Code', monospace;
  font-size: 12px; resize: vertical; outline: none;
  transition: border-color 0.2s; box-sizing: border-box;
  line-height: 1.5;
}
.curl-modal textarea:focus { border-color: #6366f1; box-shadow: 0 0 0 3px rgba(99,102,241,0.15); }
.curl-modal footer {
  padding: 16px 20px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
  display: flex; justify-content: flex-end; gap: 10px;
}
.btn-cancel {
  background: rgba(255, 255, 255, 0.05); border: 1px solid rgba(255, 255, 255, 0.1);
  color: #a6a6b2; padding: 7px 16px; border-radius: 7px;
  font-size: 12px; font-weight: 600; cursor: pointer; transition: all 0.15s;
}
.btn-cancel:hover { background: rgba(255, 255, 255, 0.1); color: #f2f7ff; }
.btn-import {
  background: linear-gradient(135deg, #4f46e5 0%, #6366f1 100%);
  border: 1px solid rgba(99,102,241,0.4);
  color: #fff; padding: 7px 18px; border-radius: 7px;
  font-size: 12px; font-weight: 600; cursor: pointer;
  box-shadow: 0 2px 10px rgba(79,70,229,0.35); transition: all 0.2s;
}
.btn-import:hover { box-shadow: 0 4px 18px rgba(79,70,229,0.55); transform: translateY(-1px); }
.btn-import:disabled { opacity: 0.4; cursor: not-allowed; transform: none; box-shadow: none; }

select option {
  background-color: #1b1b20;
  color: #f2f7ff;
}

/* Multi-Options (Tag-based tool selector) */
.multi-options-field {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.mo-selected-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.mo-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  background: rgba(99,102,241,0.2);
  border: 1px solid rgba(99,102,241,0.4);
  color: #c4b5fd;
  padding: 3px 8px 3px 10px;
  border-radius: 14px;
  font-size: 12px;
  font-weight: 500;
  white-space: nowrap;
}
.mo-tag-remove {
  background: none;
  border: none;
  color: #8b949e;
  cursor: pointer;
  font-size: 10px;
  padding: 0 2px;
  line-height: 1;
  transition: color 0.15s;
}
.mo-tag-remove:hover {
  color: #f85149;
}
.field-hint {
  display: none;
}
</style>
