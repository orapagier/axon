async function api(method, path, body) {
  const masterKey = localStorage.getItem('AXON_MASTER_KEY')
  // FormData bodies must let fetch set the multipart boundary itself — a
  // manual Content-Type would produce an unparseable request.
  const isForm = typeof FormData !== 'undefined' && body instanceof FormData
  const headers = isForm ? {} : { 'Content-Type': 'application/json' }
  if (masterKey) headers['Authorization'] = `Bearer ${masterKey}`

  const opts = { method, headers }
  if (body !== undefined) opts.body = isForm ? body : JSON.stringify(body)

  const r = await fetch('/api' + path, opts)

  if (r.status === 401) {
    localStorage.removeItem('AXON_MASTER_KEY')
    window.location.reload()
    throw new Error('Unauthorized')
  }

  const text = await r.text()
  let data = null
  if (text) {
    try {
      data = JSON.parse(text)
    } catch {
      data = null
    }
  }

  if (!r.ok) {
    // JSON error bodies keep the { ok:false, error } contract callers rely on.
    if (data && typeof data === 'object') return data
    throw new Error(`Request failed (${r.status} ${r.statusText})`)
  }

  return data ?? {}
}

export const get = (path) => api('GET', path)
export const post = (path, body) => api('POST', path, body)
export const postForm = (path, formData) => api('POST', path, formData)
export const put = (path, body) => api('PUT', path, body)
export const del = (path) => api('DELETE', path)
