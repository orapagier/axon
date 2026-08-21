<script setup>
import { toasts, dismissToast } from '../lib/toast.js'
</script>

<template>
  <div
    class="toast-host"
    aria-live="polite"
  >
    <TransitionGroup name="toast-slide">
      <div
        v-for="t in toasts"
        :key="t.id"
        class="toast-card"
        :class="{ error: !t.ok }"
        role="status"
        @click="dismissToast(t.id)"
      >
        <span
          class="toast-dot"
          aria-hidden="true"
        />
        <span class="toast-msg">{{ t.msg }}</span>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-host {
  position: fixed;
  right: 16px;
  bottom: 16px;
  z-index: 3000;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
  pointer-events: none;
}

.toast-card {
  pointer-events: auto;
  display: flex;
  align-items: flex-start;
  gap: 9px;
  max-width: 380px;
  padding: 11px 15px;
  border-radius: var(--r-lg);
  border: 1px solid color-mix(in srgb, var(--accent) 32%, var(--glass-border));
  background: var(--glass-bg);
  backdrop-filter: var(--glass-filter);
  -webkit-backdrop-filter: var(--glass-filter);
  color: var(--text);
  font-size: 0.78rem;
  line-height: 1.45;
  box-shadow: var(--glass-highlight), var(--shadow-lg);
  cursor: pointer;
}

.toast-card.error {
  border-color: color-mix(in srgb, var(--red) 45%, transparent);
}

.toast-dot {
  flex-shrink: 0;
  width: 8px;
  height: 8px;
  border-radius: 999px;
  margin-top: 5px;
  background: var(--accent);
}

.toast-card.error .toast-dot {
  background: var(--red);
}

.toast-msg {
  white-space: pre-wrap;
  word-break: break-word;
  display: -webkit-box;
  -webkit-line-clamp: 4;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.toast-slide-enter-active,
.toast-slide-leave-active {
  transition: opacity 0.28s var(--ease-out), transform 0.28s var(--ease-fluid);
}

.toast-slide-enter-from,
.toast-slide-leave-to {
  opacity: 0;
  transform: translateX(16px) scale(0.96);
}

.toast-slide-move {
  transition: transform 0.18s var(--ease-out);
}
</style>
