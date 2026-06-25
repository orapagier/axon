<script setup>
/**
 * CanvasNode - Main node component; UX inspired by n8n, independent implementation
 * Wraps the node content and manages handles/toolbar
 */
import { computed, provide, ref, toRef, watch, nextTick } from 'vue'
import { Position, Handle } from '@vue-flow/core'
import { CanvasNodeKey, CanvasConnectionMode, NodeConnectionTypes } from '../../lib/canvas/constants.js'
import { useNodeConnections } from '../../composables/useNodeConnections.js'
import { createCanvasConnectionHandleString, insertSpacersBetweenEndpoints } from '../../lib/canvas/utils.js'
import { NODE_TYPES, getNodeOutputs } from '../../lib/nodes.js'
import CanvasNodeDefault from './nodes/CanvasNodeDefault.vue'
import CanvasNodeToolbar from './CanvasNodeToolbar.vue'

const props = defineProps({
  id: { type: String, required: true },
  data: { type: Object, required: true },
  selected: { type: Boolean, default: false },
  readOnly: { type: Boolean, default: false },
  hovered: { type: Boolean, default: false },
  renaming: { type: Boolean, default: false },
})

const emit = defineEmits([
  'delete',
  'run',
  'select',
  'toggle',
  'activate',
  'deactivate',
  'open:contextmenu',
  'update',
  'focus',
  'replace:node',
  'add',
  'rename',
])

// Create reactive refs for provide
const idRef = toRef(props, 'id')
const dataRef = toRef(props, 'data')
const labelRef = computed(() => props.data.label || props.data.name || '')
const nodeTypeName = computed(() => {
  const type = props.data.node_type || props.data.type
  return NODE_TYPES[type]?.displayName || type
})
// The Stimulus node is the workflow entry point — running it executes the
// whole workflow, not a single step. We show a persistent "play" affordance
// on it so the workflow is runnable straight from its start node.
const isEntryNode = computed(() => {
  const type = props.data.node_type || props.data.type
  return type === 'trigger' || type === 'circadian' || type === 'stimulus'
})
const selectedRef = toRef(props, 'selected')
const readOnlyRef = toRef(props, 'readOnly')

// Provide canvas node context
provide(CanvasNodeKey, {
  id: idRef,
  data: dataRef,
  label: labelRef,
  selected: selectedRef,
  readOnly: readOnlyRef,
})

// Node connections
const inputs = computed(() => props.data.inputs || [])
// Nodes with dynamic outputs (e.g. Switch) derive their handles from the live
// config, so adding/removing a rule instantly adds/removes an output handle —
// no need to re-create the node. Everything else uses its stored outputs.
const outputs = computed(() => {
  const type = props.data.node_type || props.data.type
  if (NODE_TYPES[type]?.dynamicOutputs) {
    const labels = getNodeOutputs(type, props.data.config || {}) || []
    return labels.map((label, index) => ({
      type: NodeConnectionTypes.Main,
      required: false,
      index,
      label,
    }))
  }
  return props.data.outputs || []
})
const connections = computed(() => props.data.connections || { input: {}, output: {} })

const {
  mainInputs,
  mainOutputs,
  nonMainInputs,
  nonMainOutputs,
  requiredNonMainInputs,
} = useNodeConnections({ inputs, outputs, connections })

// Computed class for node state
const classes = computed(() => ({
  'canvas-node': true,
  selected: props.selected,
  hovered: props.hovered,
  disabled: props.data.disabled,
  running: props.data.execution?.running,
  waiting: props.data.execution?.waiting,
  success: props.data.execution?.status === 'success',
  error: props.data.execution?.status === 'error',
}))

// Handle mapping functions
function createEndpointMapping({ mode, position, offsetAxis }) {
  return (endpoint, index, endpoints) => {
    if (!endpoint) return null // Spacer

    const handleId = createCanvasConnectionHandleString({
      mode,
      type: endpoint.type,
      index: endpoint.index,
    })

    const handleType = mode === CanvasConnectionMode.Input ? 'target' : 'source'

    const offsetValue = position === Position.Bottom
      ? `${25 + 20 * (3 * index)}px`
      : `${(100 / (endpoints.filter(Boolean).length + 1)) * (endpoints.filter(Boolean).indexOf(endpoint) + 1)}%`

    return {
      ...endpoint,
      handleId,
      handleType,
      position,
      offset: { [offsetAxis]: offsetValue },
    }
  }
}

// Mapped inputs with handles
const mappedInputs = computed(() => {
  const mainMapping = createEndpointMapping({
    mode: CanvasConnectionMode.Input,
    position: Position.Left,
    offsetAxis: 'top',
  })

  const nonMainMapping = createEndpointMapping({
    mode: CanvasConnectionMode.Input,
    position: Position.Bottom,
    offsetAxis: 'left',
  })

  const nonMainWithSpacers = insertSpacersBetweenEndpoints(nonMainInputs.value, requiredNonMainInputs.value.length)

  return [
    ...mainInputs.value.map(mainMapping),
    ...nonMainWithSpacers.map(nonMainMapping),
  ].filter(Boolean)
})

// Mapped outputs with handles
const mappedOutputs = computed(() => {
  const mainMapping = createEndpointMapping({
    mode: CanvasConnectionMode.Output,
    position: Position.Right,
    offsetAxis: 'top',
  })

  const nonMainMapping = createEndpointMapping({
    mode: CanvasConnectionMode.Output,
    position: Position.Top,
    offsetAxis: 'left',
  })

  return [
    ...mainOutputs.value.map(mainMapping),
    ...nonMainOutputs.value.map(nonMainMapping),
  ].filter(Boolean)
})

// Event handlers
function onDelete() {
  emit('delete', props.id)
}

function onRun() {
  emit('run', props.id)
}

function onToggle() {
  emit('toggle', props.id)
}

function onActivate(event) {
  emit('activate', props.id, event)
}

function onDeactivate() {
  emit('deactivate', props.id)
}

function onOpenContextMenu(event) {
  emit('open:contextmenu', props.id, event, 'node-right-click')
}

function onUpdate(parameters) {
  emit('update', props.id, parameters)
}

function onAdd(handleId) {
  emit('add', { nodeId: props.id, handleId })
}

watch(() => props.selected, (value) => {
  emit('select', props.id, value)
})

const tempName = ref('')
const renameInput = ref(null)

watch(() => props.renaming, (isRenaming) => {
  if (isRenaming) {
    tempName.value = labelRef.value
    nextTick(() => {
      renameInput.value?.focus()
      renameInput.value?.select()
    })
  }
})

function commitRename() {
  if (props.renaming) {
    emit('rename', { id: props.id, name: tempName.value })
  }
}

function cancelRename() {
  emit('rename', { id: props.id, name: labelRef.value })
}
</script>

<template>
  <div :class="classes" :data-node-id="id" :data-node-type="data.type">
    <div class="node-wrapper">
      <!-- Output Handles -->
      <template v-for="source in mappedOutputs" :key="source.handleId">
        <div class="handle-wrapper" :style="source.offset" :class="source.position">
          <Handle
            :id="source.handleId"
            type="source"
            :position="source.position"
            :connectable="!readOnly"
            @click.stop="onAdd(source.handleId)"
          />
          <span v-if="source.label" class="handle-label" :class="'handle-label--' + source.label">
            {{ source.label === 'true' ? '✓' : (source.label === 'false' ? '✗' : source.label) }}
          </span>
        </div>
      </template>

      <!-- Input Handles -->
      <template v-for="target in mappedInputs" :key="target.handleId">
        <Handle
          :id="target.handleId"
          type="target"
          :position="target.position"
          :style="target.offset"
          :connectable="!readOnly"
        />
      </template>

      <!-- Node Toolbar -->
      <CanvasNodeToolbar
        :read-only="readOnly"
        @delete="onDelete"
        @run="onRun"
        @toggle="onToggle"
        @open:contextmenu="$emit('open:contextmenu', id, $event, 'node-button')"
      />

      <!-- Node Content (The 84x84 Box) -->
      <CanvasNodeDefault
        @activate="onActivate"
        @deactivate="onDeactivate"
        @run="onRun"
        @open:contextmenu="onOpenContextMenu"
        @replace:node="$emit('replace:node', id)"
      />

      <!-- Floating Run Button on Hover (non-entry nodes only; the entry node
           gets its own persistent play button below) -->
      <Transition name="fade-scale">
        <button
          v-if="hovered && !readOnly && !isEntryNode && !data.execution?.running"
          class="node-run-button"
          title="Run this node"
          @click.stop="onRun"
        >
          <svg viewBox="0 0 24 24" width="20" height="20">
            <path fill="currentColor" d="M8 5v14l11-7z"/>
          </svg>
        </button>
      </Transition>

      <!-- Persistent Play Button on the Stimulus/trigger entry node.
           Running it executes the whole workflow (not a single step), so it's
           always visible and labeled accordingly. -->
      <Transition name="fade-scale">
        <button
          v-if="isEntryNode && !readOnly"
          class="node-run-button node-workflow-run"
          :class="{ 'is-running': data.execution?.running }"
          :title="data.execution?.running ? 'Workflow running…' : 'Run workflow'"
          :disabled="!!data.execution?.running"
          @click.stop="onRun"
        >
          <svg v-if="!data.execution?.running" viewBox="0 0 24 24" width="20" height="20">
            <path fill="currentColor" d="M8 5v14l11-7z"/>
          </svg>
          <svg v-else class="spin" viewBox="0 0 24 24" width="18" height="18">
            <path fill="currentColor" d="M12 4V1L8 5l4 4V6c3.31 0 6 2.69 6 6 0 1.01-.25 1.97-.7 2.8l1.46 1.46C19.54 15.03 20 13.57 20 12c0-4.42-3.58-8-8-8zm0 14c-3.31 0-6-2.69-6-6 0-1.01.25-1.97.7-2.8L5.24 7.74C4.46 8.97 4 10.43 4 12c0 4.42 3.58 8 8 8v3l4-4-4-4v3z"/>
          </svg>
        </button>
      </Transition>
    </div>

    <!-- Node Name & Type (Outside the box) -->
    <div class="node-info-labels">
      <div v-if="renaming" class="node-rename-wrapper">
        <input
          ref="renameInput"
          v-model="tempName"
          class="node-rename-input"
          @blur="commitRename"
          @keyup.enter="commitRename"
          @keyup.esc="cancelRename"
          @click.stop
          @mousedown.stop
        />
      </div>
      <template v-else>
        <div class="node-label-main">{{ labelRef }}</div>
        <div v-if="labelRef && labelRef !== nodeTypeName" class="node-type-label">{{ nodeTypeName }}</div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.canvas-node {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 0;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
}

.node-wrapper {
  position: relative;
  width: var(--canvas-node--width, 100px);
  height: var(--canvas-node--height, 100px);
}

.canvas-node :deep(.vue-flow__handle) {
  width: 12px;
  height: 12px;
  background: #ffffff;
  border: 3px solid rgba(255, 255, 255, 0.3);
  border-radius: 50%;
  opacity: 0;
  transition: opacity 0.2s ease, transform 0.2s ease, border-color 0.2s ease;
  z-index: 5;
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.4);
  cursor: pointer;
}

.canvas-node :deep(.vue-flow__handle.source) {
  border-color: rgba(255, 255, 255, 0.5);
}

.canvas-node :deep(.vue-flow__handle.target) {
  border-color: rgba(255, 255, 255, 0.5);
}

.canvas-node:hover :deep(.vue-flow__handle),
.canvas-node.selected :deep(.vue-flow__handle) {
  opacity: 1;
}

.canvas-node :deep(.vue-flow__handle:hover) {
  transform: scale(1.3);
  background: #fff;
  border-color: var(--color--primary, #81e6d9);
  box-shadow: 0 0 12px var(--color--primary, #81e6d9);
}

.canvas-node :deep(.vue-flow__handle.connecting) {
  opacity: 1;
  background: var(--node-accent, #81e6d9);
  animation: pulse-handle 1s ease-in-out infinite;
}

@keyframes pulse-handle {
  0%, 100% { transform: scale(1); box-shadow: 0 0 5px var(--node-accent); }
  50% { transform: scale(1.2); box-shadow: 0 0 15px var(--node-accent); }
}

/* Placement adjustments for Handles to sit on the edge neatly */
.canvas-node :deep(.vue-flow__handle.left) { left: -6px; }
.canvas-node :deep(.vue-flow__handle.right) { right: -6px; }
.canvas-node :deep(.vue-flow__handle.top) { top: -6px; }
.canvas-node :deep(.vue-flow__handle.bottom) { bottom: -6px; }

/* Node Labels */
.node-info-labels {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  max-width: 200px;
  pointer-events: none;
}

.node-label-main {
  font-size: var(--font-size--md, 14px);
  font-weight: var(--font-weight--medium, 500);
  color: #e5e7eb;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  text-overflow: ellipsis;
  line-height: 1.3;
}

.node-type-label {
  font-size: var(--font-size--xs, 12px);
  color: rgba(255, 255, 255, 0.35);
  margin-top: 2px;
}

/* Rename UI */
.node-rename-wrapper {
  margin-top: -2px;
  z-index: 200;
}

.node-rename-input {
  background: #1a1b26;
  border: 1px solid var(--color--primary, #81e6d9);
  border-radius: 6px;
  color: #fff;
  font-size: 14px;
  font-weight: 500;
  padding: 4px 8px;
  width: 160px;
  text-align: center;
  outline: none;
  box-shadow: 0 0 15px rgba(129, 230, 217, 0.3);
}

/* Floating Run Button */
.node-run-button {
  position: absolute;
  top: -12px;
  right: -12px;
  width: 32px;
  height: 32px;
  border-radius: 50%;
  background: var(--color--success, #50fa7b);
  color: #0f1117;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 2px solid #0f1117;
  cursor: pointer;
  z-index: 150;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4), 0 0 15px rgba(80, 250, 123, 0.3);
  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

.node-run-button:hover {
  transform: scale(1.15);
  background: #fff;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5), 0 0 20px rgba(80, 250, 123, 0.5);
}

.node-run-button:active {
  transform: scale(0.95);
}

/* Persistent play button on the Stimulus/trigger entry node */
.node-workflow-run {
  top: -14px;
  right: -14px;
  width: 30px;
  height: 30px;
  /* No hover gate — this one is always shown so the workflow is runnable. */
  opacity: 1;
  background: linear-gradient(135deg, #22c55e, #16a34a);
  border-color: #0f1117;
}

.node-workflow-run:hover {
  transform: scale(1.12);
  background: linear-gradient(135deg, #4ade80, #22c55e);
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5), 0 0 22px rgba(80, 250, 123, 0.6);
}

.node-workflow-run.is-running,
.node-workflow-run:disabled {
  background: linear-gradient(135deg, #64748b, #475569);
  cursor: progress;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

.node-workflow-run .spin {
  animation: node-run-spin 0.9s linear infinite;
}

@keyframes node-run-spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

/* Run Button Transition */
.fade-scale-enter-active,
.fade-scale-leave-active {
  transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

.fade-scale-enter-from,
.fade-scale-leave-to {
  opacity: 0;
  transform: scale(0.5) translate(10px, -10px);
}

/* Handle wrapper for labeled outputs (IF node) */
.handle-wrapper {
  position: absolute;
  display: flex;
  align-items: center;
  z-index: 5;
}
.handle-wrapper.right {
  right: -6px;
}
.handle-wrapper :deep(.vue-flow__handle) {
  position: relative !important;
  left: auto !important;
  right: auto !important;
  top: auto !important;
  bottom: auto !important;
  transform: none !important;
}

.handle-label {
  position: absolute;
  left: 16px;
  font-size: 10px;
  font-weight: 700;
  border-radius: 4px;
  padding: 1px 4px;
  pointer-events: none;
  white-space: nowrap;
  opacity: 0;
  transition: opacity 0.2s ease;
  /* Neutral default so dynamic Switch labels (case N / custom / default) are
     legible; true/false override below. */
  color: #c7d2fe;
  background: rgba(99, 102, 241, 0.15);
}
.canvas-node:hover .handle-label,
.canvas-node.selected .handle-label {
  opacity: 1;
}
.handle-label--true {
  color: #50fa7b;
  background: rgba(80, 250, 123, 0.15);
}
.handle-label--false {
  color: #ff5555;
  background: rgba(255, 85, 85, 0.15);
}
</style>
