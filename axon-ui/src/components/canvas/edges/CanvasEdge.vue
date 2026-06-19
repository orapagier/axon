<script setup>
/**
 * CanvasEdge - Custom edge component matching n8n's CanvasEdge
 * Features smooth bezier curves and edge toolbar
 */
import { computed, ref } from 'vue'
import { BaseEdge, EdgeLabelRenderer, getBezierPath, useVueFlow } from '@vue-flow/core'

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

// Calculate the bezier path for the edge using n8n-style curves
const edgePath = computed(() => {
  const [path] = getBezierPath({
    sourceX: props.sourceX,
    sourceY: props.sourceY,
    sourcePosition: props.sourcePosition,
    targetX: props.targetX,
    targetY: props.targetY,
    targetPosition: props.targetPosition,
    curvature: 0.5, // n8n uses 0.5 curvature for smooth curves
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
        class="edge-toolbar diode-container"
        :class="{ 'is-active': localHovered || selected }"
        :style="{
          transform: `translate(-50%, -50%) translate(${edgeCenter.x}px, ${edgeCenter.y}px)`,
          pointerEvents: localHovered || selected ? 'auto' : 'none',
        }"
        @mouseenter="onMouseEnter"
        @mouseleave="onMouseLeave"
      >
        <!-- The "Focus Dot" that appears when not hovered -->
        <div class="focus-handle"></div>

        <!-- The full Diode UI that scales up -->
        <div class="diode-body">
          <button
            class="edge-btn diode-anode add"
            title="Add node between"
            @click="onAddClick"
          >
            <span class="symbol">+</span>
          </button>
          
          <div class="diode-glass">
            <div class="diode-filament"></div>
          </div>

          <button
            class="edge-btn diode-cathode delete"
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
  transition: all 0.3s cubic-bezier(0.34, 1.56, 0.64, 1);
}

.focus-handle {
  width: 6px;
  height: 6px;
  background: #6366f1;
  border-radius: 50%;
  opacity: 0.3;
  box-shadow: 0 0 8px rgba(99, 102, 241, 0.4);
  transition: all 0.2s ease;
}

.is-active .focus-handle {
  transform: scale(0);
  opacity: 0;
}

.diode-body {
  display: flex;
  align-items: center;
  background: rgba(18, 29, 36, 0.95);
  backdrop-filter: blur(16px);
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 20px;
  padding: 3px;
  box-shadow:
    0 8px 32px rgba(0, 0, 0, 0.8),
    0 0 15px rgba(99, 102, 241, 0.15);
  height: 32px;
  transform: scale(0);
  opacity: 0;
  transition: all 0.3s cubic-bezier(0.34, 1.56, 0.64, 1);
  position: absolute;
}

.is-active .diode-body {
  transform: scale(1);
  opacity: 1;
}

.diode-glass {
  width: 28px;
  height: 14px;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.06), rgba(255, 255, 255, 0.14));
  margin: 0 6px;
  border-radius: 3px;
  position: relative;
  overflow: hidden;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.diode-filament {
  position: absolute;
  top: 50%;
  left: 0;
  width: 100%;
  height: 2px;
  background: linear-gradient(90deg, 
    rgba(6, 182, 212, 0.2), 
    #06b6d4, 
    #6366f1, 
    #f43f5e,
    rgba(244, 63, 94, 0.2)
  );
  filter: blur(0.5px);
  box-shadow: 0 0 10px rgba(99, 102, 241, 0.6);
  opacity: 0.8;
}

.edge-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  background: transparent;
  border: none;
  border-radius: 50%;
  cursor: pointer;
  transition: all 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  color: var(--text);
}

.diode-anode {
  background: linear-gradient(135deg, #22d3ee, #0891b2);
  box-shadow: 0 0 12px rgba(34, 211, 238, 0.4);
}

.diode-anode:hover {
  transform: scale(1.2) rotate(90deg);
  box-shadow: 0 0 20px rgba(34, 211, 238, 0.8);
  background: linear-gradient(135deg, #67e8f9, #22d3ee);
}

.diode-cathode {
  background: linear-gradient(135deg, #fb7185, #e11d48);
  box-shadow: 0 0 12px rgba(251, 113, 133, 0.4);
}

.diode-cathode:hover {
  transform: scale(1.2) rotate(-90deg);
  box-shadow: 0 0 20px rgba(251, 113, 133, 0.8);
  background: linear-gradient(135deg, #fda4af, #fb7185);
}

.symbol {
  font-size: 20px;
  line-height: 1;
  font-weight: 700;
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.4);
}

.edge-btn:active {
  transform: scale(0.9);
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
