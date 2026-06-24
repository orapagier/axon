<script setup>
/**
 * CanvasBackground - Custom background component; UX inspired by n8n, independent implementation
 */
import { computed } from 'vue'

const props = defineProps({
  viewport: {
    type: Object,
    default: () => ({ x: 0, y: 0, zoom: 1 }),
  },
  striped: {
    type: Boolean,
    default: false,
  },
  gap: {
    type: Number,
    default: 20,
  },
  size: {
    type: Number,
    default: 1.5,
  },
})

// Calculate pattern offset based on viewport position
const patternOffset = computed(() => {
  const { x, y, zoom } = props.viewport
  return {
    x: x % (props.gap * zoom),
    y: y % (props.gap * zoom),
  }
})

// Calculate scaled gap based on zoom
const scaledGap = computed(() => {
  return props.gap * props.viewport.zoom
})
</script>

<template>
  <div class="canvas-background" :class="{ striped: props.striped }">
    <!-- Dots Pattern -->
    <svg
      class="dots-pattern"
      width="100%"
      height="100%"
      xmlns="http://www.w3.org/2000/svg"
    >
      <defs>
        <pattern
          id="dotPattern"
          :width="scaledGap"
          :height="scaledGap"
          patternUnits="userSpaceOnUse"
          :x="patternOffset.x"
          :y="patternOffset.y"
        >
          <circle
            :cx="scaledGap / 2"
            :cy="scaledGap / 2"
            :r="size * viewport.zoom"
            fill="currentColor"
            opacity="0.15"
          />
        </pattern>
      </defs>
      <rect width="100%" height="100%" fill="url(#dotPattern)" />
    </svg>

    <!-- Grid Pattern (visible when zoomed in) -->
    <svg
      v-if="viewport.zoom > 0.5"
      class="grid-pattern"
      width="100%"
      height="100%"
      xmlns="http://www.w3.org/2000/svg"
    >
      <defs>
        <pattern
          id="gridPattern"
          :width="scaledGap * 5"
          :height="scaledGap * 5"
          patternUnits="userSpaceOnUse"
          :x="patternOffset.x"
          :y="patternOffset.y"
        >
          <path
            :d="`M ${scaledGap * 5} 0 L 0 0 0 ${scaledGap * 5}`"
            fill="none"
            stroke="currentColor"
            stroke-width="0.5"
            opacity="0.08"
          />
        </pattern>
      </defs>
      <rect width="100%" height="100%" fill="url(#gridPattern)" />
    </svg>

    <!-- Striped Pattern (for read-only mode) -->
    <div v-if="striped" class="striped-overlay">
      <svg width="100%" height="100%" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <pattern
            id="stripePattern"
            width="20"
            height="20"
            patternUnits="userSpaceOnUse"
            patternTransform="rotate(45)"
          >
            <line
              x1="0"
              y1="0"
              x2="0"
              y2="20"
              stroke="currentColor"
              stroke-width="1"
              opacity="0.03"
            />
          </pattern>
        </defs>
        <rect width="100%" height="100%" fill="url(#stripePattern)" />
      </svg>
    </div>
  </div>
</template>

<style scoped>
.canvas-background {
  position: absolute;
  inset: 0;
  pointer-events: none;
  z-index: 0;
  color: var(--canvas--color--dots, #6b7280);
}

.dots-pattern,
.grid-pattern {
  position: absolute;
  inset: 0;
}

.striped-overlay {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.striped .striped-overlay {
  animation: stripeScroll 20s linear infinite;
}

@keyframes stripeScroll {
  from {
    background-position: 0 0;
  }
  to {
    background-position: 20px 20px;
  }
}
</style>
