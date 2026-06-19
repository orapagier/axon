<script setup>
import { toasts, dismissToast } from '../lib/toast.js'
</script>

<template>
  <div class="toast-container">
    <TransitionGroup name="toast">
      <div
        v-for="t in toasts"
        :key="t.id"
        class="toast"
        :class="{ error: !t.ok }"
        role="status"
        title="Click to dismiss"
        @click="dismissToast(t.id)"
      >
        {{ t.msg }}
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-container {
  position: fixed;
  bottom: 24px;
  right: 24px;
  z-index: 10000;
  display: flex;
  flex-direction: column;
  gap: 8px;
  pointer-events: none;
}

.toast {
  padding: 12px 20px;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 8px;
  box-shadow: var(--shadow-lg);
  color: var(--text);
  font-size: 14px;
  backdrop-filter: blur(10px);
  pointer-events: auto;
  cursor: pointer;
}

.toast.error {
  border-color: var(--red);
  color: var(--red);
}

.toast-enter-active,
.toast-leave-active {
  transition: all 0.3s ease;
}

.toast-enter-from {
  opacity: 0;
  transform: translateY(20px);
}

.toast-leave-to {
  opacity: 0;
  transform: translateX(100%);
}
</style>
