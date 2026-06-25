<script setup>
/**
 * CanvasConnectionLine - Custom connection line when dragging
 */
import { computed } from 'vue'

const props = defineProps({
  sourceX: { type: Number, required: true },
  sourceY: { type: Number, required: true },
  targetX: { type: Number, required: true },
  targetY: { type: Number, required: true },
})

// Straight drag line, matching the straight committed edges.
const path = computed(
  () => `M ${props.sourceX} ${props.sourceY} L ${props.targetX} ${props.targetY}`,
)
</script>

<template>
  <g class="canvas-connection-line">
    <path
      :d="path"
      fill="none"
      class="connection-path"
    />
  </g>
</template>

<style scoped>
.connection-path {
  stroke: var(--teal, #2c9b8d);
  stroke-width: 2;
  stroke-dasharray: 5;
  animation: dash 0.5s linear infinite;
}

@keyframes dash {
  from {
    stroke-dashoffset: 10;
  }
  to {
    stroke-dashoffset: 0;
  }
}
</style>
