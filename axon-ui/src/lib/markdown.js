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

// Auth for /api files travels in the Authorization header, never in the URL —
// a key in a URL leaks to history, proxy/server logs and Referer. Plain
// <a href> and <img src> cannot set headers, so ChatPage fetches the bytes
// with auth and swaps in a blob URL (see lib/secureFile.js): clicks on
// /api/download links are intercepted, and images below carry data-axon-src
// for post-render hydration. Absolute http(s) sources — e.g. a companion's
// own /public URL — pass through untouched and must never receive the key.
function imageTag(alt, href) {
  return href.startsWith('/api/')
    ? `<img class="md-image" data-axon-src="${href}" alt="${alt}" loading="lazy">`
    : `<img class="md-image" src="${href}" alt="${alt}" loading="lazy">`
}

function renderInline(text) {
  let out = escapeHtml(text)

  // `inline code`
  out = out.replace(/`([^`\n]+)`/g, '<code class="md-inline-code">$1</code>')

  // **bold**
  out = out.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')

  // ![alt](url) -> inline image, so screenshots and generated charts are
  // visible in the bubble instead of being a link the user has to download.
  //
  // MUST run before the link rule below: that pattern also matches the
  // `[alt](url)` half of an image, which would leave a stray "!" and render
  // the picture as a plain link. Same href restrictions as links.
  out = out.replace(
    /!\[([^\]\n]*)\]\((https?:\/\/[^)\s]+|\/(?!\/)[^)\s]*)\)/g,
    (_m, alt, href) => imageTag(alt, href)
  )

  // [label](https://url) or [label](/relative/path) — a single leading slash
  // is allowed (same-origin links like /api/download), but not `//host/...`,
  // which browsers treat as protocol-relative and would let a hallucinated
  // link jump off-origin.
  out = out.replace(
    /\[([^\]\n]+)\]\((https?:\/\/[^)\s]+|\/(?!\/)[^)\s]*)\)/g,
    (_m, label, href) => `<a href="${href}" target="_blank" rel="noopener noreferrer">${label}</a>`
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
