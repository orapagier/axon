import { ref } from 'vue'
import { addNotification } from './notifications.js'

// Transient popup toasts — quick feedback for direct user actions (saves,
// deletes, validation). Rendered by <ToastHost> in App.vue. These do NOT go
// to the notification bell; only `notifyBell` records history there.
export const toasts = ref([])

let nextId = 0

export function toast(msg, ok = true) {
  const id = ++nextId
  toasts.value.push({ id, msg, ok })
  // Errors linger longer so they can actually be read before fading.
  setTimeout(() => dismissToast(id), ok ? 3500 : 7000)
}

export function dismissToast(id) {
  toasts.value = toasts.value.filter((t) => t.id !== id)
}

// Review-worthy events (agent/model-router errors, background watcher
// notifications): recorded in the bell for later review AND flashed as a
// toast so they reach the user's attention immediately.
export function notifyBell(msg, ok = false) {
  addNotification(msg, ok)
  toast(msg, ok)
}
