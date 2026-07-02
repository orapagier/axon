// Minimal markdown renderer for chat bubbles. All input is HTML-escaped
// before any transform runs, and links are restricted to http(s), so the
// output is safe to bind with v-html. Designed for containers that use
// `white-space: pre-wrap` — newlines are kept as-is rather than turned
// into <br>/<p> tags.

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ))
}

// Agent-generated download links carry no credentials, but every /api
// route sits behind require_auth, so a plain <a href> navigation would
// 401. Append the master key the same way FilesPage does. The href has
// already been through escapeHtml, so the key is escaped to match.
function withApiKey(href) {
  if (!href.startsWith('/api/download?')) return href
  const key = typeof localStorage !== 'undefined'
    ? localStorage.getItem('AXON_MASTER_KEY')
    : null
  if (!key) return href
  return `${href}&amp;api_key=${escapeHtml(encodeURIComponent(key))}`
}

function renderInline(text) {
  let out = escapeHtml(text)

  // `inline code`
  out = out.replace(/`([^`\n]+)`/g, '<code class="md-inline-code">$1</code>')

  // **bold**
  out = out.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')

  // [label](https://url) or [label](/relative/path) — a single leading slash
  // is allowed (same-origin links like /api/download), but not `//host/...`,
  // which browsers treat as protocol-relative and would let a hallucinated
  // link jump off-origin.
  out = out.replace(
    /\[([^\]\n]+)\]\((https?:\/\/[^)\s]+|\/(?!\/)[^)\s]*)\)/g,
    (_m, label, href) => `<a href="${withApiKey(href)}" target="_blank" rel="noopener noreferrer">${label}</a>`
  )

  // # Headings -> bold lines (pre-wrap keeps them on their own line)
  out = out.replace(/^#{1,6}[ \t]+(.+)$/gm, '<strong class="md-heading">$1</strong>')

  return out
}

export function renderMarkdown(text) {
  if (!text) return ''
  const chunks = String(text).split('```')
  let html = ''
  for (let i = 0; i < chunks.length; i++) {
    if (i % 2 === 1) {
      // Fenced code block; drop a leading language tag line if present.
      let code = chunks[i]
      const nl = code.indexOf('\n')
      if (nl !== -1 && /^[\w+-]*[ \t]*$/.test(code.slice(0, nl))) code = code.slice(nl + 1)
      html += `<pre class="md-code"><code>${escapeHtml(code.replace(/\n$/, ''))}</code></pre>`
    } else {
      html += renderInline(chunks[i])
    }
  }
  return html
}
