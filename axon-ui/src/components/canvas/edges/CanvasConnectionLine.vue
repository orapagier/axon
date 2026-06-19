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

// Calculate bezier curve control points
const path = computed(() => {
  const sourceX = props.sourceX
  const sourceY = props.sourceY
  const targetX = props.targetX
  const targetY = props.targetY

  // Control points for bezier curve
  const controlPointOffset = Math.abs(targetX - sourceX) * 0.5

  const c1x = sourceX + controlPointOffset
  const c1y = sourceY
  const c2x = targetX - controlPointOffset
  const c2y = targetY

  return `M ${sourceX} ${sourceY} C ${c1x} ${c1y}, ${c2x} ${c2y}, ${targetX} ${targetY}`
})
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
