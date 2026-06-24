<script setup>
/**
 * CanvasNodeStatusIcons - Status indicator icons for nodes
 * UX inspired by n8n; independent implementation.
 */
import { useCanvasNode } from '../../../../composables/useCanvasNode.js'

const { executionRunning, executionWaiting, hasPinnedData, hasRunData, hasExecutionErrors } = useCanvasNode()
</script>

<template>
  <div class="status-icons">
    <!-- Running indicator -->
    <div v-if="executionRunning" class="status-icon running" title="Running">
      <svg viewBox="0 0 24 24" width="12" height="12">
        <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="2" stroke-dasharray="31.4" stroke-dashoffset="0">
          <animateTransform
            attributeName="transform"
            type="rotate"
            from="0 12 12"
            to="360 12 12"
            dur="1s"
            repeatCount="indefinite"
          />
        </circle>
      </svg>
    </div>

    <!-- Waiting indicator -->
    <div v-else-if="executionWaiting" class="status-icon waiting" title="Waiting">
      <svg viewBox="0 0 24 24" width="12" height="12">
        <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="2" stroke-dasharray="31.4" stroke-dashoffset="20">
          <animateTransform
            attributeName="transform"
            type="rotate"
            from="0 12 12"
            to="360 12 12"
            dur="3s"
            repeatCount="indefinite"
          />
        </circle>
      </svg>
    </div>

    <!-- Error indicator -->
    <div v-else-if="hasExecutionErrors" class="status-icon error" title="Error">
      <svg viewBox="0 0 24 24" width="12" height="12">
        <path fill="currentColor" d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z"/>
      </svg>
    </div>

    <!-- Success indicator -->
    <div v-else-if="hasRunData" class="status-icon success" title="Success">
      <svg viewBox="0 0 24 24" width="12" height="12">
        <path fill="currentColor" d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/>
      </svg>
    </div>

    <!-- Pinned data indicator -->
    <div v-if="hasPinnedData" class="status-icon pinned" title="Pinned Data">
      <svg viewBox="0 0 24 24" width="12" height="12">
        <path fill="currentColor" d="M16 9V4h2V2H6v2h2v5l-2 2v5h5v6h2v-6h5v-5l-2-2z"/>
      </svg>
    </div>
  </div>
</template>

<style scoped>
.status-icons {
  display: flex;
  gap: 4px;
  align-items: center;
}

.status-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.5);
  color: white;
}

.status-icon.running {
  color: #f59e0b;
}

.status-icon.waiting {
  color: #60a5fa;
}

.status-icon.success {
  color: #22c55e;
}

.status-icon.error {
  color: #ef4444;
}

.status-icon.pinned {
  color: #a855f7;
}
</style>
