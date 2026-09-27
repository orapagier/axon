import { describe, it, expect } from 'vitest'

const { renderMarkdown } = await import('../src/lib/markdown.js')

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

describe('renderMarkdown download links', () => {
  // The master key must never appear in a URL (history/logs/Referer leak).
  // Authenticated bytes are fetched by lib/secureFile.js instead — markdown
  // output must stay credential-free.
  it('renders /api/download links with no credentials', () => {
    const out = renderMarkdown('[Download f.pdf](/api/download?path=data%2Ffiles%2Ff.pdf)')
    expect(out).toContain('href="/api/download?path=data%2Ffiles%2Ff.pdf"')
    expect(out).not.toContain('api_key')
  })

  it('does not add credentials to other links', () => {
    const out = renderMarkdown('[x](https://example.com) [y](/api/files/staging)')
    expect(out).not.toContain('api_key')
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

describe('renderMarkdown images', () => {
  it('renders ![alt](url) as an inline image', () => {
    const out = renderMarkdown('![shot.png](https://windows.example.com/public/shot.png)')
    expect(out).toContain('<img class="md-image"')
    expect(out).toContain('src="https://windows.example.com/public/shot.png"')
    expect(out).toContain('alt="shot.png"')
    // The "!" must be consumed, not left dangling before a link.
    expect(out).not.toContain('!<')
    expect(out).not.toContain('<a href')
  })

  it('hydrates /api/download images via data-axon-src but not remote ones', () => {
    // A local /api image cannot send an Authorization header as <img src>, so
    // it is emitted as data-axon-src for ChatPage to hydrate with a blob URL.
    const local = renderMarkdown('![a.png](/api/download?path=data%2Ffiles%2Fa.png)')
    expect(local).toContain('data-axon-src="/api/download?path=data%2Ffiles%2Fa.png"')
    expect(local).not.toMatch(/\ssrc="/)
    expect(local).not.toContain('api_key')

    // A companion's own public URL is unauthenticated by design and loads
    // directly — the dashboard master key must never ride to another origin.
    const remote = renderMarkdown('![a.png](https://windows.example.com/public/a.png)')
    expect(remote).toContain('src="https://windows.example.com/public/a.png"')
    expect(remote).not.toContain('data-axon-src')
    expect(remote).not.toContain('api_key')
  })

  it('still renders ordinary links after an image', () => {
    const out = renderMarkdown('![p](/api/download?path=p.png)\n\n[Download p](/api/download?path=p.png)')
    expect(out).toContain('<img class="md-image"')
    expect(out).toContain('<a href="/api/download?path=p.png')
    expect(out).toContain('>Download p</a>')
  })

  it('does not allow protocol-relative or javascript image sources', () => {
    expect(renderMarkdown('![x](//evil.com/a.png)')).not.toContain('<img')
    expect(renderMarkdown('![x](javascript:alert(1))')).not.toContain('<img')
  })

  it('escapes markup in alt text', () => {
    const out = renderMarkdown('![<script>alert(1)</script>](/api/download?path=a.png)')
    expect(out).not.toContain('<script>')
    expect(out).toContain('&lt;script&gt;')
  })
})
