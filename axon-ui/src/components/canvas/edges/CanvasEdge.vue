<script setup>
/**
 * CanvasEdge - Custom edge component matching n8n's CanvasEdge
 * Features smooth bezier curves and edge toolbar
 */
import { computed, ref } from 'vue'
import { BaseEdge, EdgeLabelRenderer, getStraightPath, useVueFlow } from '@vue-flow/core'

const props = defineProps({
  id: { type: String, required: true },
  sourceX: { type: Number, required: true },
  sourceY: { type: Number, required: true },
  targetX: { type: Number, required: true },
  targetY: { type: Number, required: true },
  sourcePosition: { type: String, required: true },
  targetPosition: { type: String, required: true },
  markerEnd: { type: String, default: '' },
  style: { type: Object, default: () => ({}) },
  selected: { type: Boolean, default: false },
  readOnly: { type: Boolean, default: false },
  hovered: { type: Boolean, default: false },
  bringToFront: { type: Boolean, default: false },
  data: { type: Object, default: () => ({}) },
  isRunning: { type: Boolean, default: false },
})

const emit = defineEmits(['add', 'delete', 'update:label:hovered'])

const { removeEdges } = useVueFlow()

const localHovered = ref(false)
let hoverTimeout = null

// Straight-line path: a single direct line from the source output to the target
// input, so each wire visibly runs straight to the node it feeds (with an
// arrowhead at the target end via markerEnd).
const edgePath = computed(() => {
  const [path] = getStraightPath({
    sourceX: props.sourceX,
    sourceY: props.sourceY,
    targetX: props.targetX,
    targetY: props.targetY,
  })
  return path
})

// Calculate the center of the edge for the label/toolbar
const edgeCenter = computed(() => {
  const centerX = (props.sourceX + props.targetX) / 2
  const centerY = (props.sourceY + props.targetY) / 2
  return { x: centerX, y: centerY }
})

// Edge styles based on state (matching n8n colors)
const edgeStyles = computed(() => ({
  ...props.style,
  stroke: props.selected
    ? 'var(--canvas--edge--color--hover, rgba(220, 220, 220, 0.7))'
    : props.hovered
      ? 'var(--canvas--edge--color--hover, rgba(220, 220, 220, 0.7))'
      : 'var(--canvas--edge--color, rgba(180, 180, 180, 0.5))',
  strokeWidth: props.selected ? 3 : 2,
  transition: 'stroke 0.15s ease, stroke-width 0.15s ease',
}))

// Z-index based on hover state
const zIndex = computed(() => (props.bringToFront ? 1000 : 1))

function onEdgeClick(event) {
  event.stopPropagation()
}

function onAddClick(event) {
  event.stopPropagation()
  emit('add', {
    source: props.data?.source,
    target: props.data?.target,
    sourceHandle: props.data?.sourceHandle,
    targetHandle: props.data?.targetHandle,
  })
}

function onDeleteClick(event) {
  event.stopPropagation()
  removeEdges(props.id)
  emit('delete', {
    source: props.data?.source,
    target: props.data?.target,
    sourceHandle: props.data?.sourceHandle,
    targetHandle: props.data?.targetHandle,
  })
}

function onMouseEnter() {
  if (hoverTimeout) clearTimeout(hoverTimeout)
  localHovered.value = true
  emit('update:label:hovered', true)
}

function onMouseLeave() {
  if (hoverTimeout) clearTimeout(hoverTimeout)
  hoverTimeout = setTimeout(() => {
    localHovered.value = false
    emit('update:label:hovered', false)
  }, 1500)
}
</script>

<template>
  <g
    class="canvas-edge"
    :class="{ selected, hovered, 'bring-to-front': bringToFront }"
    :style="{ zIndex }"
    @mouseenter="onMouseEnter"
    @mouseleave="onMouseLeave"
  >
    <!-- Invisible thicker edge for easier hover hit area -->
    <path
      :d="edgePath"
      stroke="transparent"
      stroke-width="30"
      fill="none"
      class="edge-interaction-layer"
      @click="onEdgeClick"
    />
    
    <BaseEdge
      :id="id"
      :path="edgePath"
      :marker-end="markerEnd"
      :style="edgeStyles"
      @click="onEdgeClick"
    />

    <!-- Neurotransmitter Particles (The "Flow" - re-mountable for fresh animation) -->
    <template v-if="isRunning">
      <path
        v-for="i in 3"
        :key="'p' + i"
        :d="edgePath"
        class="neuro-particle is-pulsing"
        :class="['p' + i]"
        fill="none"
      />
    </template>

    <!-- Edge toolbar (visible on hover/selection) -->
    <EdgeLabelRenderer v-if="!readOnly">
      <div
        class="edge-toolbar edge-controls"
        :class="{ 'is-active': localHovered || selected }"
        :style="{
          transform: `translate(-50%, -50%) translate(${edgeCenter.x}px, ${edgeCenter.y}px)`,
          pointerEvents: localHovered || selected ? 'auto' : 'none',
        }"
        @mouseenter="onMouseEnter"
        @mouseleave="onMouseLeave"
      >
        <!-- Small dot shown at rest so the connection is discoverable -->
        <div class="focus-handle"></div>

        <!-- Minimal +/× controls. Kept small visually but the hit area is
             generous (transparent padding around each button) so they stay
             easy to click. -->
        <div class="controls-body">
          <button
            class="edge-btn edge-btn-add"
            title="Add node between"
            @click="onAddClick"
          >
            <span class="symbol">+</span>
          </button>

          <button
            class="edge-btn edge-btn-delete"
            title="Delete connection"
            @click="onDeleteClick"
          >
            <span class="symbol">×</span>
          </button>
        </div>
      </div>
    </EdgeLabelRenderer>
  </g>
</template>

<style scoped>
.canvas-edge {
  cursor: pointer;
}

.canvas-edge :deep(.vue-flow__edge-path) {
  stroke-linecap: round;
}

.edge-toolbar {
  position: absolute;
  z-index: 1000;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
}

.focus-handle {
  width: 5px;
  height: 5px;
  background: #6366f1;
  border-radius: 50%;
  opacity: 0.35;
  box-shadow: 0 0 6px rgba(99, 102, 241, 0.4);
  transition: all 0.18s ease;
  position: absolute;
}

.is-active .focus-handle {
  transform: scale(0);
  opacity: 0;
}

.controls-body {
  display: flex;
  align-items: center;
  gap: 3px;
  background: rgba(18, 29, 36, 0.9);
  backdrop-filter: blur(8px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 12px;
  padding: 2px;
  box-shadow: 0 2px 10px rgba(0, 0, 0, 0.5);
  transform: scale(0);
  opacity: 0;
  transition: all 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
  position: absolute;
}

.is-active .controls-body {
  transform: scale(1);
  opacity: 1;
}

.edge-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  /* Visual size kept small (16px), but generous transparent padding extends
     the hit area so the button stays easy to click. */
  width: 16px;
  height: 16px;
  padding: 7px;
  box-sizing: content-box;
  background: transparent;
  border: none;
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease, transform 0.1s ease;
  color: rgba(255, 255, 255, 0.85);
}

.edge-btn-add:hover {
  background: rgba(34, 211, 238, 0.2);
  color: #67e8f9;
  transform: scale(1.08);
}

.edge-btn-delete:hover {
  background: rgba(251, 113, 133, 0.2);
  color: #fda4af;
  transform: scale(1.08);
}

.edge-btn:active {
  transform: scale(0.92);
}

.symbol {
  font-size: 13px;
  line-height: 1;
  font-weight: 700;
  pointer-events: none;
}

/* Neuro-Flow Animation Styles - Premium Discrete Pulse Wave */
.neuro-particle {
  stroke: #2c9b8d;
  stroke-linecap: round;
  pointer-events: none;
  opacity: 0;
  filter: drop-shadow(0 0 12px #2c9b8d) drop-shadow(0 0 4px #fff);
  stroke-dasharray: 20, 120; /* Longer particles */
}

/* When edge is active, particles flow continuously */
.neuro-particle.is-pulsing {
  animation: discrete-pulse 1s linear infinite; /* Faster flow */
}

.neuro-particle.p1 { 
  stroke-width: 8; 
  stroke-dasharray: 25, 150;
}
.neuro-particle.p2 { 
  animation-delay: 0.15s;
  stroke-width: 5; 
  stroke-dasharray: 15, 120;
  stroke: #fff;
}
.neuro-particle.p3 { 
  animation-delay: 0.3s;
  stroke-width: 4; 
  stroke-dasharray: 10, 100;
  stroke: #bef3ec;
}

@keyframes discrete-pulse {
  0% { 
    stroke-dashoffset: 200; 
    opacity: 0; 
  }
  20% { 
    opacity: 1; 
  }
  80% { 
    opacity: 1; 
  }
  100% { 
    stroke-dashoffset: 0; 
    opacity: 0; 
  }
}
</style>
