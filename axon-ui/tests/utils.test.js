import { describe, it, expect } from 'vitest'
import { timeAgo, fmtTokens, fmtBytes, safeJsonParse } from '../src/lib/utils.js'

describe('timeAgo', () => {
  it('handles empty and invalid input', () => {
    expect(timeAgo(null)).toBe('—')
    expect(timeAgo('')).toBe('—')
    expect(timeAgo('not-a-date')).toBe('—')
  })

  it('formats past and future timestamps', () => {
    const twoMinAgo = new Date(Date.now() - 2 * 60 * 1000).toISOString()
    expect(timeAgo(twoMinAgo)).toBe('2m ago')
    const inOneHour = new Date(Date.now() + 90 * 60 * 1000).toISOString()
    expect(timeAgo(inOneHour)).toBe('in 1h')
  })
})

describe('fmtTokens', () => {
  it('formats below and above 1k', () => {
    expect(fmtTokens(999)).toBe('999')
    expect(fmtTokens(1500)).toBe('1.5k')
  })
})

describe('fmtBytes', () => {
  it('formats bytes, KB and MB', () => {
    expect(fmtBytes(0)).toBe('—')
    expect(fmtBytes(512)).toBe('512 B')
    expect(fmtBytes(2048)).toBe('2.0 KB')
    expect(fmtBytes(3 * 1048576)).toBe('3.0 MB')
  })
})

describe('safeJsonParse', () => {
  it('parses valid JSON and falls back on garbage', () => {
    expect(safeJsonParse('{"a":1}', {})).toEqual({ a: 1 })
    expect(safeJsonParse('{oops', 'fallback')).toBe('fallback')
    expect(safeJsonParse(null, 42)).toBe(42)
    expect(safeJsonParse({ already: 'parsed' }, null)).toEqual({ already: 'parsed' })
  })
})
