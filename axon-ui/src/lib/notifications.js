import { ref } from 'vue'

const STORAGE_KEY = 'AXON_NOTIFICATIONS'
const MAX_NOTIFICATIONS = 200

function load() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : []
  } catch {
    return []
  }
}

export const notifications = ref(load())

function persist() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(notifications.value))
  } catch {
    // localStorage full/unavailable — notifications still work for this session
  }
}

let id = notifications.value.reduce((max, n) => Math.max(max, n.id), 0)

export function addNotification(msg, ok = true) {
  notifications.value.unshift({ id: ++id, msg, ok, read: false, ts: Date.now() })
  if (notifications.value.length > MAX_NOTIFICATIONS) {
    notifications.value.length = MAX_NOTIFICATIONS
  }
  persist()
}

export function markRead(notifId) {
  const n = notifications.value.find((x) => x.id === notifId)
  if (n && !n.read) {
    n.read = true
    persist()
  }
}

export function markAllRead() {
  let changed = false
  for (const n of notifications.value) {
    if (!n.read) {
      n.read = true
      changed = true
    }
  }
  if (changed) persist()
}

export function deleteNotification(notifId) {
  notifications.value = notifications.value.filter((x) => x.id !== notifId)
  persist()
}

export function clearAllNotifications() {
  notifications.value = []
  persist()
}
