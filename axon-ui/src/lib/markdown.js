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

function renderInline(text) {
  let out = escapeHtml(text)

  // `inline code`
  out = out.replace(/`([^`\n]+)`/g, '<code class="md-inline-code">$1</code>')

  // **bold**
  out = out.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')

  // [label](https://url)
  out = out.replace(
    /\[([^\]\n]+)\]\((https?:\/\/[^)\s]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>'
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
