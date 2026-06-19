export function timeAgo(iso) {
  if (!iso) return '—'
  const ts = new Date(iso).getTime()
  if (Number.isNaN(ts)) return '—'
  const diff = Date.now() - ts
  const s = Math.floor(Math.abs(diff) / 1000)
  const fmt = (v) => (diff >= 0 ? `${v} ago` : `in ${v}`)
  if (s < 60) return fmt(`${s}s`)
  if (s < 3600) return fmt(`${Math.floor(s / 60)}m`)
  if (s < 86400) return fmt(`${Math.floor(s / 3600)}h`)
  return fmt(`${Math.floor(s / 86400)}d`)
}

export function fmtTokens(n) {
  return n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n)
}

export function fmtBytes(n) {
  if (!n) return '—'
  if (n < 1024) return n + ' B'
  if (n < 1048576) return (n / 1024).toFixed(1) + ' KB'
  return (n / 1048576).toFixed(1) + ' MB'
}

export function safeJsonParse(s, def) {
  if (s == null) return def
  if (typeof s !== 'string') return s
  try {
    const v = JSON.parse(s)
    return v != null ? v : def
  } catch {
    return def
  }
}
