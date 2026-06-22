<script setup>
/**
 * WorkflowCanvas - Main canvas wrapper based on n8n's WorkflowCanvas
 * Provides the full canvas experience with proper data mapping
 */
import { computed, ref, toRef, watch, nextTick } from 'vue'
import {
  VueFlow,
  useVueFlow,
  PanelPosition,
  MarkerType,
  getRectOfNodes,
} from '@vue-flow/core'
import { MiniMap } from '@vue-flow/minimap'
import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'

// Canvas components
import CanvasBackground from './canvas/CanvasBackground.vue'
import CanvasControlButtons from './canvas/CanvasControlButtons.vue'
import CanvasNode from './canvas/CanvasNode.vue'
import CanvasEdge from './canvas/edges/CanvasEdge.vue'
import CanvasConnectionLine from './canvas/edges/CanvasConnectionLine.vue'

// Composables
import { provideCanvas, createCanvasContext } from '../composables/useCanvas.js'
import { GRID_SIZE } from '../lib/canvas/constants.js'

const props = defineProps({
  nodes: { type: Array, required: true },
  edges: { type: Array, required: true },
  readOnly: { type: Boolean, default: false },
  executing: { type: Boolean, default: false },
  id: { type: String, default: 'workflow-canvas' },
  initialViewport: { type: Object, default: null },
  renamingNodeId: { type: String, default: null },
})

const emit = defineEmits([
  'node-select',
  'node-deselect',
  'connect',
  'disconnect',
  'update:nodes',
  'update:edges',
  'add-node',
  'splice-node',
  'insert-node',
  'delete-node',
  'run-node',
  'toggle-node',
  'activate-node',
  'tidy-up',
  'viewport-change',
  'node-context-menu',
  'rename',
  'add-from-handle',
])

// Create and provide canvas context (n8n-style)
const canvasContext = createCanvasContext()
provideCanvas(canvasContext)

// Vue Flow instance with proper configuration
const canvasId = props.id
const {
  getSelectedNodes,
  addSelectedNodes,
  removeSelectedNodes,
  viewportRef,
  fitView,
  fitBounds,
  zoomIn,
  zoomOut,
  zoomTo,
  setViewport,
  setCenter,
  project,
  findNode,
  onNodesInitialized,
  onNodeClick: vueFlowNodeClick,
  onConnect: vueFlowConnect,
  onNodeDragStop,
  onSelectionDragStop,
  onSelectionEnd,
  onPaneClick,
  onPaneMove,
  onPaneMoveEnd,
  onEdgeMouseEnter,
  onEdgeMouseLeave,
  onNodeMouseEnter,
  onNodeMouseLeave,
  viewport,
  dimensions,
  getNodes,
  getEdges,
  getSelectedEdges,
  removeEdges,
} = useVueFlow(canvasId)

// Sync viewport to context
watch(viewport, (v) => {
  canvasContext.viewport.value = v
}, { deep: true })

// Canvas state
const isPaneReady = ref(false)
const initialFitViewDone = ref(false)
const edgesHoveredById = ref({})
const edgesBringToFrontById = ref({})
const nodesHoveredById = ref({})
const connectingHandle = ref(null)

// Minimap visibility management (n8n-style)
const isMinimapVisible = ref(false)
const minimapHideTimeout = ref(null)
const minimapVisibilityDelay = 1000

function showMinimap() {
  if (minimapHideTimeout.value) {
    clearTimeout(minimapHideTimeout.value)
    minimapHideTimeout.value = null
  }
  isMinimapVisible.value = true
}

function hideMinimap() {
  minimapHideTimeout.value = setTimeout(() => {
    isMinimapVisible.value = false
  }, minimapVisibilityDelay)
}

function onMinimapMouseEnter() {
  showMinimap()
}

function onMinimapMouseLeave() {
  hideMinimap()
}

// Pane movement tracking
const isPaneMoving = ref(false)

function onPaneMoveHandler({ event }) {
  if (event instanceof WheelEvent) {
    isPaneMoving.value = true
    canvasContext.isPaneMoving.value = true
    showMinimap()
  }
}

function onPaneMoveEndHandler() {
  isPaneMoving.value = false
  canvasContext.isPaneMoving.value = false
  hideMinimap()
}

// Edge hover handling (n8n-style)
onEdgeMouseEnter(({ edge }) => {
  edgesBringToFrontById.value = { [edge.id]: true }
  edgesHoveredById.value = { [edge.id]: true }
})

onEdgeMouseLeave(({ edge }) => {
  edgesBringToFrontById.value = { [edge.id]: false }
  edgesHoveredById.value = { [edge.id]: false }
})

// Node hover handling
onNodeMouseEnter(({ node }) => {
  nodesHoveredById.value = { [node.id]: true }
})

onNodeMouseLeave(({ node }) => {
  nodesHoveredById.value = { [node.id]: false }
})

// Node click handling - emit select event
vueFlowNodeClick(({ event, node }) => {
  emit('node-select', node)
})

// Connection handling with proper data structure
vueFlowConnect((params) => {
  const edgeId = `edge_${Math.random().toString(36).substr(2, 9)}`
  const newEdge = {
    id: edgeId,
    source: params.source,
    target: params.target,
    sourceHandle: params.sourceHandle,
    targetHandle: params.targetHandle,
    type: 'canvas-edge',
    data: {
      source: params.source,
      target: params.target,
      sourceHandle: params.sourceHandle,
      targetHandle: params.targetHandle,
    },
  }
  emit('connect', newEdge)
})


// Node drag stop handling
onNodeDragStop((event) => {
  const updates = event.nodes.map(({ id, position }) => ({ id, position }))
  emit('update:nodes', updates)
})

onSelectionDragStop((event) => {
  const updates = event.nodes.map(({ id, position }) => ({ id, position }))
  emit('update:nodes', updates)
})

onSelectionEnd(() => {
  // Selection ended
})

// Pane click handling - deselect
onPaneClick((event) => {
  emit('node-deselect')
})

// Drag and drop handling
function onDragOver(event) {
  event.preventDefault()
}

function onDrop(event) {
  const type = event.dataTransfer.getData('application/axon-node')
  if (!type) return

  const bounds = viewportRef.value?.getBoundingClientRect()
  if (!bounds) return

  const position = project({
    x: event.clientX - bounds.left,
    y: event.clientY - bounds.top,
  })

  // Check for edge intersection for splicing
  const edges = getEdges.value
  const intersectingEdge = findIntersectingEdge(position, edges)

  if (intersectingEdge) {
    emit('splice-node', { type, position, edge: intersectingEdge })
  } else {
    emit('add-node', { type, position })
  }
}

// Find intersecting edge for splicing (n8n-style)
function findIntersectingEdge(pos, edges) {
  const threshold = 30
  const nodes = getNodes.value

  for (const edge of edges) {
    const sourceNode = nodes.find((n) => n.id === edge.source)
    const targetNode = nodes.find((n) => n.id === edge.target)
    if (!sourceNode || !targetNode) continue

    // Calculate distance from point to line
    const dist = pointToLineDistance(
      pos.x,
      pos.y,
      sourceNode.position.x,
      sourceNode.position.y,
      targetNode.position.x,
      targetNode.position.y
    )

    if (dist < threshold) return edge
  }
  return null
}

function pointToLineDistance(px, py, x1, y1, x2, y2) {
  const l2 = (x2 - x1) ** 2 + (y2 - y1) ** 2
  if (l2 === 0) return Math.sqrt((px - x1) ** 2 + (py - y1) ** 2)
  let t = ((px - x1) * (x2 - x1) + (py - y1) * (y2 - y1)) / l2
  t = Math.max(0, Math.min(1, t))
  return Math.sqrt((px - (x1 + t * (x2 - x1))) ** 2 + (py - (y1 + t * (y2 - y1))) ** 2)
}

// Zoom controls
async function onZoomIn() {
  await zoomIn()
}

async function onZoomOut() {
  await zoomOut()
}

async function onFitView() {
  await fitView({ padding: 0.2, maxZoom: 1 })
}

async function onResetZoom() {
  await zoomTo(1)
}

function onTidyUp() {
  emit('tidy-up')
}

function onNodeContextMenu(payload) {
  // Can be called from VueFlow native event {event, node} 
  // or from CanvasNode custom event (id, event, type)
  let event, node
  if (payload.event && payload.node) {
    event = payload.event
    node = payload.node
  } else {
    // Custom emit from CanvasNode: (id, event, type)
    // payload in @open:contextmenu="onNodeContextMenu" would be the first arg (id)
    // Wait, let's use a more robust way in template
    return // Handled by template-specific call below
  }

  console.log('[Canvas] Node context menu event:', node.id)
  event.preventDefault()
  emit('node-context-menu', { event, node })
}

function handleNodeCustomContextMenu(nodeId, event) {
  console.log('[Canvas] Custom node context menu:', nodeId)
  const node = findNode(nodeId)
  if (node) {
    event.preventDefault()
    emit('node-context-menu', { event, node })
  }
}

// Initialize fit view
onNodesInitialized(() => {
  if (!initialFitViewDone.value) {
    nextTick(() => {
      if (!props.initialViewport) {
        fitView({ padding: 0.2, maxZoom: 1 })
      }
      initialFitViewDone.value = true
    })
  }
  canvasContext.initialized.value = true
})

// Auto refit when the node list completely changes (like switching workflows)
watch(() => props.nodes, (newNodes, oldNodes) => {
  if (newNodes !== oldNodes && newNodes.length > 0) {
    initialFitViewDone.value = false
  }
}, { flush: 'post' })

// Node and edge type registration
const nodeTypes = {
  'canvas-node': CanvasNode,
}

const edgeTypes = {
  'canvas-edge': CanvasEdge,
}

// Default edge options (n8n-style)
const defaultEdgeOptions = {
  type: 'canvas-edge',
  animated: false,
  markerEnd: MarkerType.ArrowClosed,
}

// Keyboard shortcuts
function handleKeyDown(e) {
  if (props.readOnly) return

  // Delete selected nodes
  if ((e.key === 'Delete' || e.key === 'Backspace') && getSelectedNodes.value.length > 0) {
    if (['INPUT', 'TEXTAREA'].includes(document.activeElement.tagName)) return
    const selectedIds = getSelectedNodes.value.map((n) => n.id)
    selectedIds.forEach((id) => emit('delete-node', id))
  }

  // Zoom shortcuts
  if (e.key === '0' && (e.ctrlKey || e.metaKey)) {
    e.preventDefault()
    onResetZoom()
  }
  if (e.key === '1' && (e.ctrlKey || e.metaKey)) {
    e.preventDefault()
    onFitView()
  }
}

// Reactive helper to check if an edge should be animating based on target node state
function isEdgeAnimating(targetId) {
  const node = findNode(targetId)
  if (!node || !node.data?.execution) return false
  const exec = node.data.execution
  const isRunning = exec.running || exec.status === 'running' || exec.waiting === true
  return isRunning
}

// ── Execution helpers (script-scope so they can call each other freely) ────────

function updateNodeExecution(nodeId, execution) {
  const node = findNode(nodeId)
  if (node) {
    node.data = { ...node.data, execution }
  }
}

function updateAllNodesExecution(executionMap) {
  for (const [nodeId, execution] of Object.entries(executionMap)) {
    const node = findNode(nodeId)
    if (node) {
      node.data = { ...node.data, execution }
    }
  }
}

async function processNodeResult(result) {
  if (!result || !result.node_id) return true

  const nodeId = String(result.node_id)
  const node = findNode(nodeId)
  if (!node) return true

  const hasError = !!result.error
  const isSkipped = result.status === 'skipped'
  const finalStatus = isSkipped ? 'skipped' : (hasError ? 'error' : 'success')

  if (isSkipped) {
    // Disabled/skipped nodes: instant transition, no running flash
    updateNodeExecution(nodeId, { running: false, waiting: false, status: 'skipped' })
  } else {
    // Brief "running" flash so the user sees the node light up.
    // We do NOT replay the backend duration: the backend already spent that time
    // while we were polling, so the flash must be short to stay in sync.
    updateNodeExecution(nodeId, { running: true, waiting: false, status: 'running' })
    await new Promise((resolve) => setTimeout(resolve, 350))

    // Resolve to final state
    updateNodeExecution(nodeId, { running: false, waiting: false, status: finalStatus })
  }

  if (!hasError && !isSkipped) {
    // For branching nodes (IF/Switch), only mark the selected branch as waiting.
    // Otherwise non-selected branches can remain stuck in waiting:true forever.
    const nodeType = node.data?.node_type || node.data?.type
    const isBranchNode = nodeType === 'ifCondition' || nodeType === 'switch'
    const takenBranch = result.output?.branch
    const rawOutputIndex = result.output?.outputIndex
    const outputIndex = Number.isFinite(Number(rawOutputIndex)) ? Number(rawOutputIndex) : null

    // Mark immediate successors as "waiting" so their incoming edges start animating
    // while the backend is already processing them — this is what makes the flow feel live.
    const successorEdges = getEdges.value
      .filter(e => (typeof e.source === 'string' ? e.source : e.source?.id) === nodeId)

    successorEdges.forEach(e => {
      const tid = typeof e.target === 'string' ? e.target : e.target?.id
      if (!tid) return

      // For branch nodes: skip edges that belong to non-selected outputs
      if (isBranchNode) {
        const handle = e.sourceHandle || ''
        const normalized = handle.toLowerCase()

        if (outputIndex !== null) {
          const expectedHandle = `output_main_${outputIndex}`
          if (handle !== expectedHandle) {
            // Backward compatibility for IF nodes that used true/false labels
            const branchTaken = takenBranch === 'true' || takenBranch === true
            const legacyMatch = (branchTaken && normalized === 'true') || (!branchTaken && normalized === 'false')
            if (!legacyMatch) return
          }
        } else if (takenBranch !== undefined) {
          const edgeIsTrue = handle === 'output_main_0' || normalized === 'true'
          const edgeIsFalse = handle === 'output_main_1' || normalized === 'false'
          const branchTaken = takenBranch === 'true' || takenBranch === true
          if ((branchTaken && edgeIsFalse) || (!branchTaken && edgeIsTrue)) return
        }
      }

      const next = findNode(tid)
      // Only animate nodes not yet in a terminal state
      if (next && !['success', 'error', 'skipped'].includes(next.data?.execution?.status)) {
        updateNodeExecution(tid, { running: false, waiting: true, status: 'unknown' })
      }
    })
  }

  // Honour stop-on-fail: if this node errored and continueOnFail is not set,
  // return false so runLivePlayback stops the visual sequence immediately.
  const continueOnFail = node.data?.continueOnFail === true
  if (hasError && !continueOnFail) {
    return false
  }

  return true
}

// Expose methods for parent component
defineExpose({
  getNodes: () => getNodes.value,
  getEdges: () => getEdges.value,
  getSelectedNodes: () => getSelectedNodes.value,
  getSelectedEdges: () => getSelectedEdges.value,
  fitView: onFitView,
  resetZoom: onResetZoom,
  zoomTo,
  getViewport: () => viewport.value,
  setViewport: (v) => setViewport(v),
  // Update node execution state
  updateNodeExecution,
  // Batch update all nodes' execution states
  updateAllNodesExecution,
  // Push a data change into Vue Flow's internal store so it renders immediately.
  // The parent's nodes array stays the save source of truth, but Vue Flow keeps
  // its own node objects (and execution updates sever the shared data reference),
  // so rename/replace must be mirrored here or they won't show until a reload.
  updateNodeData(nodeId, data, { replace = false } = {}) {
    const node = findNode(nodeId)
    if (!node) return false
    node.data = replace ? data : { ...node.data, ...data }
    return true
  },
  // Internal method to animate a single node result
  processNodeResult,

  // Sequentially playback the results directly in the canvas
  async runVisualPlayback(nodeResults) {
    if (!nodeResults || nodeResults.length === 0) return
    for (const result of nodeResults) {
      const shouldContinue = await processNodeResult(result)
      if (!shouldContinue) break
    }
  },

  // Live playback that waits for results to appear in a reactive source
  async runLivePlayback(getLatestResults, isActiveRun, isBackendDone) {
    let processedCount = 0
    const MAX_PLAYBACK_MS = 10 * 60 * 1000 // 10-minute safety limit
    const startTime = Date.now()
    let lastProgressTime = Date.now()
    let lastStallLog = 0

    console.log('[LivePlayback] Started')

    while (true) {
      const elapsed = Date.now() - startTime

      // Safety timeout — prevent infinite spinning even if backend/polling breaks
      if (elapsed > MAX_PLAYBACK_MS) {
        console.warn('[LivePlayback] Safety timeout reached (10min), stopping.')
        const allNodes = getNodes.value
        allNodes.forEach(node => {
          const exec = node.data?.execution
          if (exec && (exec.running || exec.waiting)) {
            updateNodeExecution(node.id, {
              running: false,
              waiting: false,
              status: 'error',
            })
          }
        })
        break
      }

      // Emergency stop if user cancels
      if (!isActiveRun()) {
        console.log('[LivePlayback] Run cancelled by user')
        const allNodes = getNodes.value
        allNodes.forEach(node => {
          const exec = node.data?.execution
          if (exec && (exec.running || exec.waiting)) {
            updateNodeExecution(node.id, {
              running: false,
              waiting: false,
              status: (exec.status === 'running' || exec.status === 'unknown') ? 'unknown' : exec.status,
            })
          }
        })
        break
      }

      const results = getLatestResults()
      if (processedCount < results.length) {
        const result = results[processedCount]
        console.log(`[LivePlayback] Processing node ${processedCount + 1}/${results.length}: ${result.node_id} (${result.node_type}) status=${result.status}`)
        const shouldContinue = await processNodeResult(result)
        processedCount++
        lastProgressTime = Date.now()
        if (!shouldContinue) break
      } else {
        // No new results yet. Are we done in the backend?
        if (isBackendDone()) {
          console.log(`[LivePlayback] Complete. Processed ${processedCount} nodes in ${elapsed}ms`)
          break
        }

        // Stall detection: warn every 10s when no progress
        const stallDuration = Date.now() - lastProgressTime
        if (stallDuration > 10000 && Date.now() - lastStallLog > 10000) {
          console.warn(`[LivePlayback] Stall ${Math.round(stallDuration / 1000)}s: processed=${processedCount}, available=${results.length}, backendDone=${isBackendDone()}, elapsed=${Math.round(elapsed / 1000)}s`)
          lastStallLog = Date.now()
        }

        // Wait a bit for next poll
        await new Promise(resolve => setTimeout(resolve, 200))
      }
    }

    // Final cleanup: reset any nodes still stuck in running/waiting state.
    // Uses VueFlow's internal getNodes (not the parent prop) to catch all nodes
    // including disabled ones that were marked waiting but never got a result.
    const allNodes = getNodes.value
    allNodes.forEach(node => {
      const exec = node.data?.execution
      if (exec && (exec.running || exec.waiting)) {
        updateNodeExecution(node.id, {
          running: false,
          waiting: false,
          status: (exec.status === 'running' || exec.status === 'unknown') ? 'unknown' : exec.status,
        })
      }
    })
  },
})
</script>

<template>
  <div
    class="canvas-container"
    @drop="onDrop"
    @dragover.prevent="onDragOver"
    @keydown="handleKeyDown"
    tabindex="0"
  >
    <VueFlow
      :id="canvasId"
      :nodes="nodes"
      :edges="edges"
      :node-types="nodeTypes"
      :edge-types="edgeTypes"
      :default-edge-options="defaultEdgeOptions"
      :fit-view-on-init="false"
      :snap-to-grid="true"
      :snap-grid="[GRID_SIZE, GRID_SIZE]"
      :min-zoom="0.1"
      :max-zoom="4"
      :selection-key-code="true"
      :zoom-activation-key-code="['Control', 'Meta']"
      :pan-activation-key-code="[' ', 'Control', 'Meta']"
      :pan-on-drag="[1]"
      pan-on-scroll
      :elevate-edges-on-select="true"
      :elevate-nodes-on-select="true"
      :class="['n8n-canvas', { 'is-executing': executing }]"
      @pane-ready="isPaneReady = true"
      @viewport-change="emit('viewport-change', $event)"
      @node-context-menu="onNodeContextMenu"
    >
      <!-- Custom Node Template -->
      <template #node-canvas-node="nodeProps">
        <CanvasNode
          v-bind="nodeProps"
          :read-only="readOnly"
          :hovered="nodesHoveredById[nodeProps.id]"
          :renaming="renamingNodeId === nodeProps.id"
          @add="emit('add-from-handle', $event)"
          @delete="emit('delete-node', $event)"
          @run="emit('run-node', $event)"
          @select="emit('node-select', findNode($event))"
          @toggle="emit('toggle-node', $event)"
          @activate="emit('activate-node', findNode($event))"
          @rename="emit('rename', $event)"
          @open:contextmenu="handleNodeCustomContextMenu"
        />
      </template>

      <!-- Custom Edge Template -->
      <template #edge-canvas-edge="edgeProps">
        <CanvasEdge
          v-bind="edgeProps"
          :marker-end="`url(#arrowhead-${edgeProps.id})`"
          :read-only="readOnly"
          :hovered="edgesHoveredById[edgeProps.id]"
          :bring-to-front="edgesBringToFrontById[edgeProps.id]"
          :is-running="isEdgeAnimating(edgeProps.target)"
          @add="emit('insert-node', { edgeId: edgeProps.id, ...$event })"
          @delete="emit('disconnect', $event)"
        />
      </template>

      <!-- Custom Connection Line -->
      <template #connection-line="connectionLineProps">
        <CanvasConnectionLine v-bind="connectionLineProps" />
      </template>

      <!-- Arrow Marker Definitions -->
      <defs>
        <marker
          v-for="edge in edges"
          :id="`arrowhead-${edge.id}`"
          :key="`marker-${edge.id}`"
          markerWidth="12"
          markerHeight="12"
          refX="10"
          refY="6"
          orient="auto"
        >
          <path
            d="M0,0 L0,12 L12,6 z"
            fill="var(--canvas--edge--color, rgba(180, 180, 180, 0.5))"
          />
        </marker>
      </defs>

      <!-- Custom Background -->
      <CanvasBackground :viewport="viewport" :striped="readOnly" />

      <!-- Minimap (n8n-style fade in/out) -->
      <Transition name="minimap">
        <MiniMap
          v-show="isMinimapVisible"
          class="canvas-minimap"
          :width="200"
          :height="120"
          :position="PanelPosition.BottomLeft"
          pannable
          zoomable
          :node-border-radius="8"
          :node-class-name="(node) => `minimap-node-${node.type}`"
          @mouseenter="onMinimapMouseEnter"
          @mouseleave="onMinimapMouseLeave"
        />
      </Transition>

      <!-- Control Buttons -->
      <CanvasControlButtons
        :zoom="viewport.zoom"
        :read-only="readOnly"
        position="bottom-left"
        @zoom-in="onZoomIn"
        @zoom-out="onZoomOut"
        @zoom-to-fit="onFitView"
        @reset-zoom="onResetZoom"
        @tidy-up="onTidyUp"
      />
    </VueFlow>
  </div>
</template>

<style scoped>
.canvas-container {
  width: 100%;
  height: 100%;
  min-height: 0;
  background: #0f1117;
  position: relative;
  outline: none;
}

:deep(.vue-flow) {
  background: transparent !important;
}

:deep(.vue-flow__node) {
  border: none !important;
  background: transparent !important;
  padding: 0 !important;
  box-shadow: none !important;
  z-index: 1;
}

:deep(.vue-flow__node.selected) {
  box-shadow: none !important;
}

:deep(.vue-flow__edge-path) {
  stroke: var(--canvas--edge--color, rgba(180, 180, 180, 0.5)) !important;
  stroke-width: 2;
}

:deep(.vue-flow__edge.selected .vue-flow__edge-path) {
  stroke: var(--canvas--edge--color--hover, rgba(220, 220, 220, 0.7)) !important;
  stroke-width: 3;
}

:deep(.vue-flow__connection-path) {
  stroke: var(--canvas--edge--color--hover, rgba(220, 220, 220, 0.7)) !important;
  stroke-width: 2;
}

:deep(.vue-flow__controls) {
  display: none !important;
}

:deep(.vue-flow__minimap) {
  background: rgba(15, 17, 23, 0.9) !important;
  backdrop-filter: blur(10px);
  border: 1px solid rgba(255, 255, 255, 0.1) !important;
  border-radius: 10px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
}

/* Minimap transition */
.minimap-enter-active,
.minimap-leave-active {
  transition: opacity 0.3s ease;
}

.minimap-enter-from,
.minimap-leave-to {
  opacity: 0;
}

:deep(.vue-flow__selection) {
  background: rgba(129, 230, 217, 0.1);
  border: 1px solid rgba(129, 230, 217, 0.3);
}

:deep(.vue-flow__pane) {
  cursor: grab;
}

:deep(.vue-flow__pane:active) {
  cursor: grabbing;
}

:deep(.vue-flow__pane.dragging) {
  cursor: grabbing;
}

:deep(.vue-flow__handle) {
  opacity: 0;
  transition: opacity 0.15s ease;
}

:deep(.vue-flow__node:hover .vue-flow__handle),
:deep(.vue-flow__node.selected .vue-flow__handle) {
  opacity: 1;
}
</style>
