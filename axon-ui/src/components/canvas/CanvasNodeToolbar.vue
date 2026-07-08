<script setup>
/**
 * CanvasNodeToolbar - Node toolbar with actions
 * UX inspired by n8n; independent implementation.
 */
defineProps({
  readOnly: {
    type: Boolean,
    default: false,
  },
})

const emit = defineEmits([
  'delete',
  'run',
  'toggle',
  'open:contextmenu',
])

function onDelete() {
  emit('delete')
}

function onRun() {
  emit('run')
}

function onToggle() {
  emit('toggle')
}

function onOpenContextMenu(event) {
  emit('open:contextmenu', event)
}
</script>

<template>
  <div class="canvas-node-toolbar">
    <div class="toolbar-items">
      <button
        v-if="!readOnly"
        class="toolbar-btn"
        title="Delete Node"
        @click.stop="onDelete"
      >
        <svg
          viewBox="0 0 24 24"
          width="14"
          height="14"
        >
          <path
            fill="currentColor"
            d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"
          />
        </svg>
      </button>


      <button
        v-if="!readOnly"
        class="toolbar-btn"
        title="Toggle Enabled"
        @click.stop="onToggle"
      >
        <svg
          viewBox="0 0 24 24"
          width="14"
          height="14"
        >
          <path
            fill="currentColor"
            d="M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5zM12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5 5 2.24 5 5-2.24 5-5 5zm0-8c-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3-1.34-3-3-3z"
          />
        </svg>
      </button>

      <button
        class="toolbar-btn"
        title="More Actions"
        @click.stop="onOpenContextMenu"
      >
        <svg
          viewBox="0 0 24 24"
          width="14"
          height="14"
        >
          <path
            fill="currentColor"
            d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"
          />
        </svg>
      </button>
    </div>
  </div>
</template>

<style scoped>
.canvas-node-toolbar {
  position: absolute;
  bottom: 100%;
  left: 50%;
  transform: translateX(-50%);
  z-index: 100;
  padding-bottom: 8px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.15s ease;
}

:global(.canvas-node:hover) .canvas-node-toolbar,
:global(.canvas-node.selected) .canvas-node-toolbar,
:global(.canvas-node:focus-within) .canvas-node-toolbar {
  opacity: 1;
  pointer-events: auto;
}

.toolbar-items {
  display: flex;
  align-items: center;
  justify-content: center;
  background-color: var(--canvas--color--background, #f5f6fa);
  border-radius: var(--radius, 8px);
  pointer-events: auto;
}

.toolbar-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  background: transparent;
  border: none;
  border-radius: 6px;
  color: var(--color--foreground, #6b7280);
  cursor: pointer;
  transition: all 0.15s ease;
}

.toolbar-btn:hover {
  background: rgba(0, 0, 0, 0.1);
  color: var(--text, #374151);
}

.toolbar-btn:active {
  transform: scale(0.95);
}
</style>
