<script setup>
/**
 * CanvasNodeDefault — n8n-accurate node renderer
 * 100x100px box with conic-gradient running animation, state borders, icon centered
 */
import { computed, ref } from 'vue'
import { useCanvasNode } from '../../../composables/useCanvasNode.js'
import { NODE_TYPES } from '../../../lib/nodes.js'
import CanvasNodeStatusIcons from './parts/CanvasNodeStatusIcons.vue'

const emit = defineEmits([
  'activate',
  'deactivate',
  'open:contextmenu',
  'replace:node',
  'run',
])

import { getToolIcon } from '../../../lib/toolIcons.js'

const { id, data, label, isDisabled, isSelected, executionRunning, executionWaiting, executionStatus, hasRunData, hasExecutionErrors } = useCanvasNode()

const showTooltip = ref(false)

const isTrigger = computed(() => {
  const type = data.value?.node_type || data.value?.type
  return type === 'trigger'
})

const classes = computed(() => ({
  'node-default': true,
  selected: isSelected.value,
  disabled: isDisabled.value,
  running: executionRunning.value,
  waiting: executionWaiting.value,
  success: hasRunData.value && !hasExecutionErrors.value,
  error: hasExecutionErrors.value,
  trigger: isTrigger.value,
}))

const nodeIcon = computed(() => {
  const type = data.value?.node_type || data.value?.type
  
  // Try to get dynamic icon based on tool name (for MCP)
  // Check both 'parameters' and 'config' — backend stores tool selection in config
  const params = data.value?.parameters || data.value?.config || {}
  const dynamicIcon = getToolIcon(type, params)
  if (dynamicIcon) return dynamicIcon

  return NODE_TYPES[type]?.icon || '📦'
})

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

function openContextMenu(event) {
  emit('open:contextmenu', event)
}

function onActivate(event) {
  emit('activate', id.value, event)
}
</script>

<template>
  <div
    :class="classes"
    @dblclick.stop="onActivate"
    @contextmenu.stop.prevent="openContextMenu"
    @mouseenter="showTooltip = true"
    @mouseleave="showTooltip = false"
  >
    <div class="node-icon-wrapper">
      <img
        v-if="isImageUrl(nodeIcon)"
        :src="nodeIcon"
        class="node-icon-img"
      >
      <span
        v-else
        class="node-icon-main"
      >{{ nodeIcon }}</span>
    </div>

    <CanvasNodeStatusIcons
      v-if="!isDisabled"
      class="status-icons"
    />

    <!-- Disabled strike-through -->
    <div
      v-if="isDisabled"
      class="disabled-strike"
    />

    <slot name="handles" />
  </div>
</template>

<style scoped>
.node-default {
  position: relative;
  width: var(--canvas-node--width, 100px);
  height: var(--canvas-node--height, 100px);
  background: var(--node--color--background, #1e1f28);
  border: var(--canvas-node--border-width, 2px) solid rgba(255, 255, 255, 0.08);
  border-radius: var(--radius--lg, 16px);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

/* Trigger node: rounded left side */
.node-default.trigger {
  border-radius: var(--trigger-node--radius, 36px) var(--radius--lg, 16px)
    var(--radius--lg, 16px) var(--trigger-node--radius, 36px);
}

.node-default:hover {
  border-color: rgba(255, 255, 255, 0.2);
}

/* ── State classes (priority order, matching n8n) ── */

.node-default.selected {
  box-shadow: 0 0 0 6px var(--canvas--color--selected-transparent, rgba(129, 230, 217, 0.15));
}

.node-default.success {
  border-color: var(--color--success, #22c55e);
}

.node-default.error {
  border-color: var(--color--danger, #ef4444);
}

.node-default.disabled {
  border-color: var(--color--foreground, #6b7280);
  opacity: 0.6;
}

/* ── Running / Waiting conic-gradient animation (n8n-exact) ── */

.node-default.receiving {
  border-color: #81e6d9;
  box-shadow: 0 0 15px rgba(129, 230, 217, 0.4);
  animation: node-breathing 1.5s ease-in-out infinite;
}

@keyframes node-breathing {
  0%, 100% { box-shadow: 0 0 5px rgba(129, 230, 217, 0.2); }
  50% { box-shadow: 0 0 20px rgba(129, 230, 217, 0.6); }
}

.node-default.running,
.node-default.waiting {
  border-color: transparent;
}

.node-default.running::after,
.node-default.waiting::after {
  content: '';
  position: absolute;
  inset: -3px;
  border-radius: calc(var(--radius--lg, 16px) + 1px);
  z-index: -1;
  background: conic-gradient(
    from var(--node--gradient-angle, 0deg),
    rgba(129, 230, 217, 1),
    rgba(129, 230, 217, 1) 20%,
    rgba(129, 230, 217, 0.2) 35%,
    rgba(129, 230, 217, 0.2) 65%,
    rgba(129, 230, 217, 1) 90%,
    rgba(129, 230, 217, 1)
  );
}

.node-default.trigger.running::after,
.node-default.trigger.waiting::after {
  border-radius: calc(var(--trigger-node--radius, 36px) + 1px)
    calc(var(--radius--lg, 16px) + 1px)
    calc(var(--radius--lg, 16px) + 1px)
    calc(var(--trigger-node--radius, 36px) + 1px);
}

.node-default.running::after {
  animation: border-rotate 1.5s linear infinite, node-hit 0.4s ease-out forwards;
}

@keyframes node-hit {
  0% { transform: scale(0.95); }
  50% { transform: scale(1.05); }
  100% { transform: scale(1); }
}

.node-default.waiting::after {
  animation: border-rotate 4.5s linear infinite;
}

@property --node--gradient-angle {
  syntax: '<angle>';
  initial-value: 0deg;
  inherits: false;
}

@keyframes border-rotate {
  from { --node--gradient-angle: 0deg; }
  to { --node--gradient-angle: 360deg; }
}

/* ── Icon ── */

.node-icon-wrapper {
  font-size: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-grow: 0;
  flex-shrink: 0;
  filter: drop-shadow(0 2px 6px rgba(0, 0, 0, 0.4));
}

.node-icon-img {
    width: 48px;
    height: 48px;
    object-fit: contain;
}

.node-icon-main {
    line-height: 1;
}

/* ── Status Icons ── */

.status-icons {
  position: absolute;
  bottom: 4px;
  right: 4px;
}

/* ── Disabled Strike-through ── */

.disabled-strike {
  position: absolute;
  top: 50%;
  left: -5%;
  width: 110%;
  height: 2px;
  background: var(--color--foreground, #6b7280);
  transform: rotate(-45deg);
  transform-origin: center;
  pointer-events: none;
}
</style>
