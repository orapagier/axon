import { getBlob } from './api.js'

// Authenticated file access for plain `<a href>` / `<img src>` targets, which
// cannot attach an Authorization header — and the master key must never ride
// in a URL, where history, proxy/server logs and Referer would leak it.
// Instead the bytes are fetched as a blob and surfaced through a short-lived
// object: URL.

// Save `href` (a same-origin /api path) as a browser download.
export async function downloadFile(href, filename) {
  const blob = await getBlob(href)
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  if (filename) a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  // Revoke async: a synchronous revoke can cancel the just-triggered download
  // in some browsers.
  setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

// Hydrate `<img data-axon-src>` (emitted by markdown.js for /api images) with
// authenticated blob URLs. Call on the rendered container after DOM updates.
export function hydrateSecureImages(root) {
  if (!root) return
  for (const img of root.querySelectorAll('img[data-axon-src]')) {
    const src = img.getAttribute('data-axon-src')
    img.removeAttribute('data-axon-src')
    getBlob(src)
      .then((blob) => { img.src = URL.createObjectURL(blob) })
      .catch(() => { img.setAttribute('alt', 'image failed to load') })
  }
}

// Delegated click handler for chat-bubble containers: routes clicks on
// /api/download anchors through downloadFile(). A blob URL carries no
// Content-Disposition, so the name is taken from the path parameter.
export function onSecureLinkClick(event) {
  const a = event.target.closest?.('a')
  if (!a) return
  const href = a.getAttribute('href')
  if (!href || !href.startsWith('/api/download?')) return
  event.preventDefault()
  const decoded = new URL(href, location.origin).searchParams.get('path') || ''
  downloadFile(href, decoded.split('/').pop() || 'download')
}
