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

    const tokenButtons = wrapper.findAll('button').filter((b) => b.text() === 'Token')
    expect(tokenButtons.length).toBeGreaterThan(0)
    for (const btn of tokenButtons) {
      await btn.trigger('click')
      await wrapper.vm.$nextTick()

      // Modal content renders via <Teleport to="body">, so it's outside
      // wrapper's own root — query the real DOM directly.
      const input = document.querySelector('input[placeholder="Paste bot token…"]')
      expect(input).not.toBeNull()
      expect(input.getAttribute('type')).toBe('password')
    }
  })

  it('masks the Add Credential value input — a field named "access_token" must not render in plaintext while being typed', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const addButton = wrapper.findAll('button').find((b) => b.text().includes('Add secure credential'))
    expect(addButton).toBeTruthy()
    await addButton.trigger('click')
    await wrapper.vm.$nextTick()

    const keyInput = document.querySelector('input[placeholder^="Key"]')
    const valueInput = document.querySelector('input[placeholder="Value"]')
    expect(keyInput).not.toBeNull()
    expect(valueInput).not.toBeNull()
    expect(valueInput.getAttribute('type')).toBe('password')
  })

  it('masks the MCP connect API key input', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const openButton = wrapper.findAll('button').find((b) => b.text().includes('Connect MCP server'))
    expect(openButton).toBeTruthy()
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    const secretInput = document.querySelector('input[placeholder="••••••••••••••••"]')
    expect(secretInput).not.toBeNull()
    expect(secretInput.getAttribute('type')).toBe('password')
  })

  it('masks the SSH password input (password auth mode)', async () => {
    wrapper = mount(ServicesPage)
    await flushPromises()

    const openButton = wrapper.findAll('button').find((b) => b.text().includes('Register SSH server'))
    expect(openButton).toBeTruthy()
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    // Password field only renders once auth_type is switched off the
    // default 'key' mode. The <select> is teleported along with the rest of
    // the modal, so drive it via a native DOM event rather than VTU's find().
    const authSelect = document.querySelector('select')
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

    const openButton = wrapper.findAll('button').find((b) => b.text().includes('Add Tavily account'))
    expect(openButton).toBeTruthy()
    await openButton.trigger('click')
    await wrapper.vm.$nextTick()

    const secretInput = document.querySelector('input[placeholder="tvly-..."]')
    expect(secretInput).not.toBeNull()
    expect(secretInput.getAttribute('type')).toBe('password')
  })
})
