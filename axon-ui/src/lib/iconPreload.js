// Node icons live in /public/icons/*.png and are referenced by literal path, so
// each one is a separate HTTP request fired the moment a node first renders —
// which is why a node could briefly show the 📦 fallback before its real icon
// arrived. Eagerly fetching them at app startup warms the browser cache so the
// images are already resident by the time the canvas paints.
//
// This is the pragmatic stop-gap; the long-term fix is inline SVG icons bundled
// into the JS (no network round-trip at all).
import { NODE_TYPES } from './nodes.js'
import { TOOL_ICONS, MCP_SERVICE_ICONS } from './toolIcons.js'

function collectIconUrls() {
  const urls = new Set()
  const add = (v) => {
    if (typeof v === 'string' && v.startsWith('/')) urls.add(v)
  }
  for (const def of Object.values(NODE_TYPES)) add(def?.icon)
  for (const v of Object.values(TOOL_ICONS)) add(v)
  for (const v of Object.values(MCP_SERVICE_ICONS)) add(v)
  return urls
}

// Idempotent: re-requesting an already-cached image is a no-op for the network,
// so this is safe to call again after MCP tools dynamically extend NODE_TYPES.
export function preloadNodeIcons() {
  if (typeof window === 'undefined') return
  for (const url of collectIconUrls()) {
    const img = new Image()
    img.src = url
  }
}
