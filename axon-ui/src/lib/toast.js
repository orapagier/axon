import { addNotification } from './notifications.js'

// Popup toasts were replaced by the notification bell — this now just
// records history instead of also rendering a transient popup.
export function toast(msg, ok = true) {
  addNotification(msg, ok)
}
