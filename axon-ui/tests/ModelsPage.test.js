import { describe, it, expect, vi, afterEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ModelsPage from '../src/pages/ModelsPage.vue'

const api = vi.hoisted(() => ({
  get: vi.fn(),
  post: vi.fn().mockResolvedValue({ ok: true }),
  put: vi.fn().mockResolvedValue({ ok: true }),
  del: vi.fn().mockResolvedValue({ ok: true }),
}))
vi.mock('../src/lib/api.js', () => api)

const EXISTING_MODEL = {
  name: 'my-gpt',
  provider: 'openai',
  model_id: 'gpt-4o',
  base_url: '',
  priority: 1,
  role: '',
  max_tokens: 4096,
  status: 'available',
  enabled: true,
}

describe('ModelsPage — API key secrecy', () => {
  let wrapper

  afterEach(() => {
    wrapper?.unmount()
    wrapper = undefined
    api.get.mockReset()
  })

  it('masks the API key input in the Add/Edit modal', async () => {
    api.get.mockResolvedValue({ models: [] })
    wrapper = mount(ModelsPage)
    await flushPromises()

    await wrapper.find('.btn-save').trigger('click') // "+ Add Model"
    await wrapper.vm.$nextTick()

    // Modal content renders via <Teleport to="body">, so it's outside
    // wrapper's own root — query the real DOM directly.
    const apiKeyInput = document.querySelector('input[placeholder="••••••••••••••••"]')
    expect(apiKeyInput).not.toBeNull()
    expect(apiKeyInput.getAttribute('type')).toBe('password')
  })

  it('does not prefill an existing model\'s stored API key when editing', async () => {
    api.get.mockResolvedValue({ models: [EXISTING_MODEL] })
    wrapper = mount(ModelsPage)
    await flushPromises()

    const editButton = wrapper.findAll('button').find((b) => b.text() === 'Edit')
    expect(editButton).toBeTruthy()
    await editButton.trigger('click')
    await wrapper.vm.$nextTick()

    // The backend response for an existing model never even carries a
    // plaintext api_key field (server-side secret) — this asserts the
    // *frontend* doesn't independently invent/prefill one into the form
    // either, so there's nothing to leak if that ever changed upstream.
    const apiKeyInput = document.querySelector('input[placeholder="••••••••••••••••"]')
    expect(apiKeyInput).not.toBeNull()
    expect(apiKeyInput.value).toBe('')
  })
})
