<script setup>
import { computed } from 'vue'
import { Handle, Position } from '@vue-flow/core'

const props = defineProps({
  data: {
    type: Object,
    required: true,
  },
  selected: {
    type: Boolean,
    default: false,
  }
})

const isRunning = computed(() => props.data.execution?.status === 'running' || props.data.execution?.running)
const isSuccess = computed(() => props.data.execution?.status === 'success')
const isError = computed(() => props.data.execution?.status === 'error')
</script>

<template>
  <div 
    class="custom-node trigger-node" 
    :class="{ 
      selected: props.selected,
      running: isRunning,
      success: isSuccess,
      error: isError 
    }"
  >
    <div
      class="node-type-bar"
      style="background: var(--teal)"
    />
    <div class="node-icon">
      ⚡
    </div>
    <div class="node-content">
      <div class="node-label">
        {{ data.label || "Trigger" }}
      </div>
      <div class="node-sublabel">
        {{ data.config?.type || "Manual" }}
      </div>
    </div>
    <div
      v-if="data.execution?.status"
      class="status-indicator"
      :class="data.execution.status"
    />
    <Handle
      type="source"
      :position="Position.Right"
      class="custom-handle"
    />
  </div>
</template>

<style scoped>
@property --gradient-angle {
  syntax: '<angle>';
  initial-value: 0deg;
  inherits: false;
}

.trigger-node { 
  background: rgba(255, 255, 255, 0.85); 
  backdrop-filter: blur(12px);
  border: 1px solid rgba(0, 0, 0, 0.08); 
  border-radius: 12px; 
  width: 150px; 
  height: 64px; 
  display: flex; 
  align-items: center; 
  padding: 0 12px; 
  position: relative; 
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.2);
}

.trigger-node:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
  background: rgba(255, 255, 255, 0.9);
}

.trigger-node.selected {
  border-color: var(--teal);
  box-shadow: 0 0 0 3px rgba(129, 230, 217, 0.2), 0 8px 24px rgba(0, 0, 0, 0.4);
}

.node-type-bar { 
  position: absolute; 
  left: 0; 
  top: 12px; 
  bottom: 12px; 
  width: 4px; 
  border-radius: 0 4px 4px 0; 
  opacity: 0.8;
}

.node-icon { 
  font-size: 24px; 
  margin-right: 12px; 
  filter: drop-shadow(0 0 8px rgba(129, 230, 217, 0.4));
}

.node-label { 
  font-size: 13px; 
  font-weight: 700; 
  color: var(--text); 
  margin-bottom: 2px;
}

.node-sublabel { 
  font-size: 10px; 
  color: rgba(0, 0, 0, 0.5); 
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.status-indicator { 
  position: absolute; 
  right: 12px; 
  top: 12px; 
  width: 8px; 
  height: 8px; 
  border-radius: 50%; 
  background: #444;
  box-shadow: 0 0 8px rgba(0,0,0,0.5);
}

.status-indicator.success { background: var(--green, #50fa7b); box-shadow: 0 0 12px var(--green, #50fa7b); }
.status-indicator.error { background: var(--red, #ff5555); box-shadow: 0 0 12px var(--red, #ff5555); }
.status-indicator.running { background: var(--teal, #2c9b8d); animation: breathe 1s ease-in-out infinite; }

@keyframes breathe {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(0.8); }
}

/* Running State Animation */
.trigger-node.running::after {
  content: '';
  position: absolute;
  inset: -2px;
  border-radius: 14px;
  z-index: -1;
  background: conic-gradient(
    from var(--gradient-angle),
    var(--teal),
    rgba(129, 230, 217, 0.2) 25%,
    var(--teal) 50%,
    rgba(129, 230, 217, 0.2) 75%,
    var(--teal)
  );
  animation: rotate-gradient 1.5s linear infinite;
}

@keyframes rotate-gradient {
  from { --gradient-angle: 0deg; }
  to { --gradient-angle: 360deg; }
}

/* Connection Handle */
.custom-handle {
  width: 10px !important;
  height: 10px !important;
  background: var(--teal) !important;
  border: 2.5px solid #11131a !important;
  transition: all 0.2s;
  opacity: 0;
}

.trigger-node:hover .custom-handle,
.trigger-node.selected .custom-handle {
  opacity: 1;
}

.custom-handle:hover {
  transform: scale(1.4);
  box-shadow: 0 0 12px var(--teal);
}
</style>
