import { ref } from 'vue'
import { get, post, del } from './api.js'

// The bell is server-backed: history lives in the `notifications` table and is
// pushed live over the WS broadcast, so it survives reloads and is identical on
// every open tab.
//
// Client-only failures are the exception. Things like "voice message not sent —
// not connected" or a TTS fallback happen *because* the server is unreachable,
// so posting them would fail too. Those are kept as local rows: in-memory only,
// never synced, and dropped on reload. `local: true` marks them, and every
// mutation below branches on it.

const MAX_NOTIFICATIONS = 200

export const notifications = ref([])

let localSeq = 0

/// SQLite writes `datetime('now')` as UTC with no zone marker ("2026-07-20
/// 14:33:01"). `new Date()` would read that as *local* time and skew every
/// timestamp by the UTC offset, so normalize it to an ISO-8601 UTC string.
function parseServerTime(s) {
  if (!s) return Date.now()
  const ms = Date.parse(s.includes('T') ? s : `${s.replace(' ', 'T')}Z`)
  return Number.isNaN(ms) ? Date.now() : ms
}

/// Server row -> the flat shape the bell renders. `key` is what Vue and the
/// mutation helpers use, since local rows have no server id.
function fromServer(row) {
  const title = (row.title || '').trim()
  const message = (row.message || '').trim()
  return {
    key: `s${row.id}`,
    id: row.id,
    local: false,
    source: row.source || '',
    msg: title ? `${title}\n${message}` : message || 'Notification',
    ok: (row.level || '').toLowerCase() !== 'error',
    read: !!row.read,
    ts: parseServerTime(row.created_at),
  }
}

function trim() {
  if (notifications.value.length > MAX_NOTIFICATIONS) {
    notifications.value.length = MAX_NOTIFICATIONS
  }
}

/// Load history from the server, preserving any local-only rows already shown.
export async function loadNotifications() {
  try {
    const res = await get('/notifications')
    if (!res || !Array.isArray(res.notifications)) return
    const locals = notifications.value.filter((n) => n.local)
    notifications.value = [...locals, ...res.notifications.map(fromServer)].sort(
      (a, b) => b.ts - a.ts,
    )
    trim()
  } catch {
    // Offline or unauthorized — keep whatever is on screen and retry on the
    // next reconnect.
  }
}

let reconcileTimer = null

/// A WS push shows instantly but carries no row id, so mark/delete can't reach
/// the server for it. Refetch shortly after to swap the provisional rows for
/// real ones. Debounced so a burst of notifications costs a single request.
function scheduleReconcile() {
  clearTimeout(reconcileTimer)
  reconcileTimer = setTimeout(() => {
    loadNotifications()
  }, 600)
}

/// Handle a `notification` WS event. The server already persisted the row, so
/// this only mirrors it into the list for immediate display; the id arrives
/// with the reconciling refetch below.
export function pushWsNotification(ev) {
  const title = (ev.title || '').trim()
  const message = (ev.message || '').trim()
  notifications.value.unshift({
    key: `w${++localSeq}`,
    id: null,
    local: false,
    pending: true,
    source: ev.source || '',
    msg: title ? `${title}\n${message}` : message || 'Notification',
    ok: (ev.level || '').toLowerCase() !== 'error',
    read: false,
    ts: Date.now(),
  })
  trim()
  scheduleReconcile()
}

/// Record a client-side-only notification (see the module note). Not persisted.
export function addNotification(msg, ok = true) {
  notifications.value.unshift({
    key: `l${++localSeq}`,
    id: null,
    local: true,
    source: 'client',
    msg,
    ok,
    read: false,
    ts: Date.now(),
  })
  trim()
}

export async function markRead(key) {
  const n = notifications.value.find((x) => x.key === key)
  if (!n || n.read) return
  n.read = true
  if (n.local || n.id === null) return
  try {
    await post('/notifications/mark_read', { id: n.id })
  } catch {
    // Optimistic: the row stays read locally and re-syncs on next load.
  }
}

export async function markAllRead() {
  const hadServerRows = notifications.value.some((n) => !n.local && !n.read)
  for (const n of notifications.value) n.read = true
  if (!hadServerRows) return
  try {
    await post('/notifications/mark_read', {})
    // Rows pushed over WS have no id yet; refetch so they carry real ids.
    await loadNotifications()
  } catch {
    // Ignore — local state already reflects the intent.
  }
}

export async function deleteNotification(key) {
  const n = notifications.value.find((x) => x.key === key)
  if (!n) return
  notifications.value = notifications.value.filter((x) => x.key !== key)
  if (n.local || n.id === null) return
  try {
    await del('/notifications', { id: n.id })
  } catch {
    // Ignore — a failed delete reappears on the next load, which is correct.
  }
}

export async function clearAllNotifications() {
  notifications.value = []
  try {
    await del('/notifications', {})
  } catch {
    // Ignore.
  }
}
