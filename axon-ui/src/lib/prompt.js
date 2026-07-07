import { ref } from 'vue'

// Singleton state for the global <PromptDialog/> mounted once in App.vue.
// Call promptDialog(...) from anywhere to await a styled text input
// instead of the native window.prompt() popup.
export const promptState = ref(null)

export function promptDialog(message, defaultValue = '', options = {}) {
  return new Promise((resolve) => {
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
