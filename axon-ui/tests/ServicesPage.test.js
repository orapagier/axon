import { describe, it, expect, vi, afterEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import ServicesPage from '../src/pages/ServicesPage.vue'

// ServicesPage is the highest-value page to cover: every field on it is
// either a credential secret or gates access to one. These tests guard the
// one property that actually matters for that — secrets never render as
// plaintext in the DOM — rather than chasing full behavioral coverage.
vi.mock('../src/lib/api.js', () => ({
  get: vi.fn().mockResolvedValue({}),
  post: vi.fn().mockResolvedValue({ ok: true }),
  put: vi.fn().mockResolvedValue({ ok: true }),
  del: vi.fn().mockResolvedValue({ ok: true }),
}))

describe('ServicesPage — credential secrecy', () => {
  let wrapper

  afterEach(() => {
    wrapper?.unmount()
    wrapper = undefined
  })

  it('masks messaging bot token inputs', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const tokenInputs = wrapper.findAll('.token-input-compact')
    expect(tokenInputs.length).toBeGreaterThan(0)
    for (const input of tokenInputs) {
      expect(input.attributes('type')).toBe('password')
    }
  })

  it('masks the Add Credential value input — a field named "access_token" must not render in plaintext while being typed', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const addButton = wrapper
      .findAll('.btn-premium-action')
      .find((b) => b.text().includes('Add Secure Credential'))
    expect(addButton).toBeTruthy()
    await addButton.trigger('click')
    await wrapper.vm.$nextTick()

    // Modal content renders via <Teleport to="body">, so it's outside
    // wrapper's own root — query the real DOM directly.
    const keyInput = document.querySelector('input[placeholder^="Key"]')
    const valueInput = document.querySelector('input[placeholder="Value"]')
    expect(keyInput).not.toBeNull()
    expect(valueInput).not.toBeNull()
    expect(valueInput.getAttribute('type')).toBe('password')
  })

  it('masks the MCP connect API key input', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const openButton = wrapper
      .findAll('.btn-premium-action')
      .find((b) => b.text().includes('Connect MCP Server'))
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    const secretInput = document.querySelector('input[placeholder="••••••••••••••••"]')
    expect(secretInput).not.toBeNull()
    expect(secretInput.getAttribute('type')).toBe('password')
  })

  it('masks the SSH password input (password auth mode)', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const openButton = wrapper
      .findAll('.btn-premium-action')
      .find((b) => b.text().includes('Register SSH Server'))
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    // Password field only renders once auth_type is switched off the
    // default 'key' mode. The <select> is teleported along with the rest of
    // the modal, so drive it via a native DOM event rather than VTU's find().
    const authSelect = document.querySelector('select.select-input')
    authSelect.value = 'password'
    authSelect.dispatchEvent(new Event('change'))
    await wrapper.vm.$nextTick()

    const secretInput = document.querySelector('input[placeholder="••••••••••••••••"]')
    expect(secretInput).not.toBeNull()
    expect(secretInput.getAttribute('type')).toBe('password')
  })

  it('masks the Tavily API key input', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const openButton = wrapper
      .findAll('.btn-premium-action')
      .find((b) => b.text().includes('Add Tavily Account'))
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    const secretInput = document.querySelector('input[placeholder="tvly-..."]')
    expect(secretInput).not.toBeNull()
    expect(secretInput.getAttribute('type')).toBe('password')
  })
})
