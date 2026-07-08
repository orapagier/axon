import { ref } from 'vue'
import { addNotification } from './notifications.js'

export const toasts = ref([])

let id = 0
export function toast(msg, ok = true) {
  const t = { id: ++id, msg, ok }
  toasts.value.push(t)
  addNotification(msg, ok)
  // Errors stay visible longer so they can actually be read.
  setTimeout(() => {
    toasts.value = toasts.value.filter(x => x.id !== t.id)
  }, ok ? 3000 : 6000)
}

export function dismissToast(toastId) {
  toasts.value = toasts.value.filter(x => x.id !== toastId)
}
