import { describe, it, expect } from 'vitest'

// Vitest runs in a plain node environment; give markdown.js the browser
// localStorage it reads the master key from.
const store = new Map()
globalThis.localStorage = {
  getItem: (k) => (store.has(k) ? store.get(k) : null),
  setItem: (k, v) => store.set(k, String(v)),
  removeItem: (k) => store.delete(k),
}

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
  it('appends the master key to /api/download links', () => {
    localStorage.setItem('AXON_MASTER_KEY', 'k+y=/1')
    try {
      const out = renderMarkdown('[Download f.pdf](/api/download?path=data%2Ffiles%2Ff.pdf)')
      expect(out).toContain('href="/api/download?path=data%2Ffiles%2Ff.pdf&amp;api_key=k%2By%3D%2F1"')
    } finally {
      localStorage.removeItem('AXON_MASTER_KEY')
    }
  })

  it('leaves download links untouched when no key is stored', () => {
    const out = renderMarkdown('[Download f.pdf](/api/download?path=x.pdf)')
    expect(out).toContain('href="/api/download?path=x.pdf"')
  })

  it('does not append the key to other links', () => {
    localStorage.setItem('AXON_MASTER_KEY', 'secret')
    try {
      const out = renderMarkdown('[x](https://example.com) [y](/api/files/staging)')
      expect(out).not.toContain('secret')
    } finally {
      localStorage.removeItem('AXON_MASTER_KEY')
    }
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

  it('appends the master key to /api/download images but not to remote ones', () => {
    localStorage.setItem('AXON_MASTER_KEY', 'k123')
    try {
      const local = renderMarkdown('![a.png](/api/download?path=data%2Ffiles%2Fa.png)')
      expect(local).toContain('api_key=k123')

      // A companion's own public URL is unauthenticated by design — leaking the
      // dashboard master key to another host would be a real credential leak.
      const remote = renderMarkdown('![a.png](https://windows.example.com/public/a.png)')
      expect(remote).not.toContain('api_key')
      expect(remote).not.toContain('k123')
    } finally {
      localStorage.removeItem('AXON_MASTER_KEY')
    }
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
