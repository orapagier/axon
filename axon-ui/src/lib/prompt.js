import { ref } from 'vue'

// Singleton state for the global <PromptDialog/> mounted once in App.vue.
// Call promptDialog(...) from anywhere to await a styled text input
// instead of the native window.prompt() popup.
export const promptState = ref(null)

export function promptDialog(message, defaultValue = '', options = {}) {
  return new Promise((resolve) => {
    // A dialog opened while another is up displaces it — resolve the old one
    // as "cancelled" so its awaiting caller doesn't hang forever.
    promptState.value?.resolve(null)
    promptState.value = {
      title: options.title ?? 'Enter a value',
      message,
      value: defaultValue ?? '',
      placeholder: options.placeholder ?? '',
      confirmText: options.confirmText ?? 'Save',
      cancelText: options.cancelText ?? 'Cancel',
      resolve,
    }
  })
}
