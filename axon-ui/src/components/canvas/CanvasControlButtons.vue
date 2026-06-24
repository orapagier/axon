<script setup>
/**
 * CanvasControlButtons - Zoom and view controls; UX inspired by n8n, independent implementation
 */
const props = defineProps({
  position: {
    type: String,
    default: 'bottom-left',
  },
  zoom: {
    type: Number,
    default: 1,
  },
  readOnly: {
    type: Boolean,
    default: false,
  },
  showInteractive: {
    type: Boolean,
    default: false,
  },
})

const emit = defineEmits([
  'zoom-in',
  'zoom-out',
  'zoom-to-fit',
  'reset-zoom',
  'tidy-up',
  'toggle-zoom-mode',
])

function onZoomIn() {
  emit('zoom-in')
}

function onZoomOut() {
  emit('zoom-out')
}

function onZoomToFit() {
  emit('zoom-to-fit')
}

function onResetZoom() {
  emit('reset-zoom')
}

function onTidyUp() {
  emit('tidy-up')
}
</script>

<template>
  <div class="canvas-controls" :class="[position]">
    <div class="control-group">
      <button
        class="control-btn"
        title="Zoom In"
        data-test-id="zoom-in-button"
        @click="onZoomIn"
      >
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path fill="currentColor" d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/>
        </svg>
      </button>
      <div class="zoom-level">{{ Math.round(zoom * 100) }}%</div>
      <button
        class="control-btn"
        title="Zoom Out"
        data-test-id="zoom-out-button"
        @click="onZoomOut"
      >
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path fill="currentColor" d="M19 13H5v-2h14v2z"/>
        </svg>
      </button>
    </div>

    <div class="control-group">
      <button
        class="control-btn"
        title="Fit View"
        data-test-id="zoom-to-fit-button"
        @click="onZoomToFit"
      >
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path fill="currentColor" d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/>
        </svg>
      </button>
      <button
        class="control-btn"
        title="Reset Zoom"
        data-test-id="reset-zoom-button"
        @click="onResetZoom"
      >
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path fill="currentColor" d="M12 5V1L7 6l5 5V7c3.31 0 6 2.69 6 6s-2.69 6-6 6-6-2.69-6-6H4c0 4.42 3.58 8 8 8s8-3.58 8-8-3.58-8-8-8z"/>
        </svg>
      </button>
      <button
        v-if="!readOnly"
        class="control-btn"
        title="Tidy Up (Auto Layout)"
        data-test-id="tidy-up-button"
        @click="onTidyUp"
      >
        <svg viewBox="0 0 24 24" width="16" height="16">
          <path fill="currentColor" d="M3 13h8V3H3v10zm0 8h8v-6H3v6zm10 0h8V11h-8v10zm0-18v6h8V3h-8z"/>
        </svg>
      </button>
    </div>
  </div>
</template>

<style scoped>
.canvas-controls {
  display: flex;
  flex-direction: column;
  gap: 8px;
  position: absolute;
  z-index: 10;
}

.canvas-controls.bottom-left {
  bottom: 12px;
  left: 12px;
}

.canvas-controls.bottom-right {
  bottom: 12px;
  right: 12px;
}

@media (min-width: 768px) {
  .canvas-controls.bottom-left {
    bottom: 20px;
    left: 20px;
  }
  .canvas-controls.bottom-right {
    bottom: 20px;
    right: 20px;
  }
}

.control-group {
  display: flex;
  flex-direction: column;
  background: rgba(25, 25, 35, 0.85);
  backdrop-filter: blur(12px);
  border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
  border-radius: 10px;
  overflow: hidden;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.5);
}

.control-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 44px;
  background: transparent;
  border: none;
  color: var(--text, #e5e7eb);
  cursor: pointer;
  transition: all 0.15s ease;
}

@media (min-width: 768px) {
  .control-btn {
    width: 36px;
    height: 36px;
  }
}

.control-btn:hover {
  background: rgba(255, 255, 255, 0.1);
  color: var(--teal, #81e6d9);
}

.control-btn + .control-btn {
  border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.zoom-level {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 28px;
  font-size: 11px;
  font-weight: 500;
  color: var(--muted, #9ca3af);
  border-top: 1px solid rgba(255, 255, 255, 0.05);
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  user-select: none;
}

@media (min-width: 768px) {
  .zoom-level {
    width: 36px;
  }
}
</style>
