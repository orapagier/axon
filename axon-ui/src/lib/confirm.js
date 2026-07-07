import { ref } from 'vue'

// Singleton state for the global <ConfirmDialog/> mounted once in App.vue.
// Call confirmDialog(...) from anywhere to await a styled yes/no decision
// instead of the native window.confirm() popup.
export const confirmState = ref(null)

export function confirmDialog(message, options = {}) {
  return new Promise((resolve) => {
    confirmState.value = {
      title: options.title ?? 'Are you sure?',
      message,
      confirmText: options.confirmText ?? 'Confirm',
      cancelText: options.cancelText ?? 'Cancel',
      danger: options.danger ?? true,
      resolve,
    }
  })
}
