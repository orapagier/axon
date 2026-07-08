<script setup>
import { BaseEdge, EdgeLabelRenderer, getBezierPath, useVueFlow } from '@vue-flow/core'
import { computed } from 'vue'

const props = defineProps({
  id: { type: String, required: true },
  sourceX: { type: Number, required: true },
  sourceY: { type: Number, required: true },
  targetX: { type: Number, required: true },
  targetY: { type: Number, required: true },
  sourcePosition: { type: String, required: true },
  targetPosition: { type: String, required: true },
  data: { type: Object, default: () => ({}) },
  markerEnd: { type: String, default: '' },
  style: { type: Object, default: () => ({}) },
})

const { findEdge, emit } = useVueFlow()

const path = computed(() => getBezierPath(props))

function onEdgeClick(evt) {
  evt.stopPropagation()
  // Trigger insertion at the edge's midpoint
  const [edgePath, labelX, labelY] = path.value
  emit('insert-node', { 
    edgeId: props.id, 
    position: { x: labelX, y: labelY } 
  })
}
</script>

<template>
  <BaseEdge
    :id="id"
    :path="path[0]"
    :marker-end="markerEnd"
    :style="style"
  />

  <EdgeLabelRenderer>
    <div
      :style="{
        position: 'absolute',
        transform: `translate(-50%, -50%) translate(${path[1]}px,${path[2]}px)`,
        pointerEvents: 'all',
      }"
      class="nodrag nopan"
    >
      <button
        class="edge-button"
        @click="onEdgeClick"
      >
        <span class="plus-icon">+</span>
      </button>
    </div>
  </EdgeLabelRenderer>
</template>

<style scoped>
.edge-button {
  width: 20px;
  height: 20px;
  background: #e5e8f0;
  border: 2px solid var(--teal);
  border-radius: 50%;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--teal);
  font-weight: bold;
  font-size: 14px;
  box-shadow: 0 0 10px rgba(0, 0, 0, 0.5);
  transition: all 0.2s;
  z-index: 10;
}

.edge-button:hover {
  transform: scale(1.2);
  background: var(--teal);
  color: #11131a;
  box-shadow: 0 0 15px var(--teal);
}

.plus-icon {
  line-height: 1;
}
</style>
