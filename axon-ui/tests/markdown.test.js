import { describe, it, expect } from 'vitest'
import { renderMarkdown } from '../src/lib/markdown.js'

// This module's output is bound with v-html in chat bubbles — these tests
// lock in the escaping guarantees that make that binding safe.
describe('renderMarkdown XSS safety', () => {
  it('escapes raw HTML', () => {
    const out = renderMarkdown('<script>alert(1)</script>')
    expect(out).not.toContain('<script>')
    expect(out).toContain('&lt;script&gt;')
  })

  it('escapes HTML inside fenced code blocks', () => {
    const out = renderMarkdown('```\n<img src=x onerror=alert(1)>\n```')
    expect(out).not.toContain('<img')
    expect(out).toContain('&lt;img')
  })

  it('only links http(s) URLs', () => {
    expect(renderMarkdown('[x](https://example.com)')).toContain('href="https://example.com"')
    expect(renderMarkdown('[x](javascript:alert(1))')).not.toContain('href=')
  })
})

describe('renderMarkdown formatting', () => {
  it('renders bold, inline code and headings', () => {
    expect(renderMarkdown('**hi**')).toBe('<strong>hi</strong>')
    expect(renderMarkdown('`code`')).toBe('<code class="md-inline-code">code</code>')
    expect(renderMarkdown('# Title')).toContain('<strong class="md-heading">Title</strong>')
  })

  it('drops the language tag line in fenced blocks', () => {
    const out = renderMarkdown('```js\nconst a = 1\n```')
    expect(out).toContain('<pre class="md-code"><code>const a = 1</code></pre>')
  })

  it('returns empty string for empty input', () => {
    expect(renderMarkdown('')).toBe('')
    expect(renderMarkdown(null)).toBe('')
  })
})
