import { describe, it, expect } from 'vitest'
import { applyAccessPatterns, renameNodeInExpressions } from '../src/lib/expressionUpdates.js'

describe('applyAccessPatterns', () => {
  it('rewrites $node["Name"] references', () => {
    expect(applyAccessPatterns('{{ $node["Old Name"].json.x }}', 'Old Name', 'New Name'))
      .toBe('{{ $node["New Name"].json.x }}')
  })

  it("rewrites $('Name') references", () => {
    expect(applyAccessPatterns("$('Old').item.json", 'Old', 'New'))
      .toBe("$('New').item.json")
  })

  it('leaves unrelated expressions untouched', () => {
    expect(applyAccessPatterns('{{ $json.foo }}', 'Old', 'New')).toBe('{{ $json.foo }}')
    expect(applyAccessPatterns('plain text', 'Old', 'New')).toBe('plain text')
  })

  it('escapes regex metacharacters in node names', () => {
    expect(applyAccessPatterns('$node["A (v1.0)"].json', 'A (v1.0)', 'B'))
      .toBe('$node["B"].json')
  })
})

describe('renameNodeInExpressions', () => {
  it('rewrites references nested inside node configs', () => {
    const nodes = [
      { data: { config: { url: '{{ $node["Fetch"].json.url }}', nested: { note: '$("Fetch")' } } } },
    ]
    renameNodeInExpressions(nodes, 'Fetch', 'Fetch v2')
    expect(nodes[0].data.config.url).toBe('{{ $node["Fetch v2"].json.url }}')
  })

  it('is a no-op when labels match or args are missing', () => {
    const nodes = [{ data: { config: { a: '$node["X"].json' } } }]
    renameNodeInExpressions(nodes, 'X', 'X')
    expect(nodes[0].data.config.a).toBe('$node["X"].json')
  })
})
