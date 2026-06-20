import { ref } from 'vue'
import { get } from './api.js'

export const wsStatus = ref('connecting') // 'connected' | 'disconnected' | 'connecting'

let ws = null
let reconnectTimer = null
let keepaliveTimer = null
let eventHandler = null

function openSocket() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const masterKey = localStorage.getItem('AXON_MASTER_KEY')
  const query = masterKey ? `?api_key=${encodeURIComponent(masterKey)}` : ''
  const socket = new WebSocket(`${proto}://${location.host}/ws${query}`)
  ws = socket
  wsStatus.value = 'connecting'
  let opened = false

  socket.onopen = () => {
    if (ws !== socket) return
    opened = true
    wsStatus.value = 'connected'
  }
  socket.onclose = () => {
    if (ws !== socket) return
    wsStatus.value = 'disconnected'
    clearTimeout(reconnectTimer)
    // A handshake rejected for a bad/stale master key closes without ever
    // opening. The browser can't read the 401 off a failed WS upgrade, so
    // confirm with a REST auth probe: get() clears the stale key and bounces
    // to login on 401, while a transient outage throws and falls through to a
    // normal reconnect. Without this, a wrong key reconnects every 3s forever.
    if (!opened) {
      get('/settings').catch(() => {})
    }
    reconnectTimer = setTimeout(openSocket, 3000)
  }
  socket.onerror = () => socket.close()
  socket.onmessage = (e) => {
    if (ws !== socket) return
    try {
      const ev = JSON.parse(e.data)
      if (eventHandler) eventHandler(ev)
    } catch { }
  }
}

export function connectWs(onEvent) {
  eventHandler = onEvent

  // Reuse the live connection if there is one — callers may invoke this on
  // every page mount.
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return
  }

  clearTimeout(reconnectTimer)
  openSocket()

  if (!keepaliveTimer) {
    keepaliveTimer = setInterval(() => {
      if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'ping' }))
    }, 25000)
  }
}

export function wsSend(payload) {
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(payload))
    return true
  }
  return false
}
