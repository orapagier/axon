import { afterEach, describe, expect, it } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import DocsPage from '../src/pages/DocsPage.vue'
import { headerSearchFor } from '../src/lib/headerSearch.js'

let wrapper
async function openDocs() {
  wrapper = mount(DocsPage, { attachTo: document.body })
  await flushPromises()
  return wrapper
}
afterEach(() => wrapper?.unmount())

describe('Documentation reader', () => {
  it('navigates articles and moves keyboard focus to the new heading', async () => {
    await openDocs()
    expect(wrapper.findAll('article')).toHaveLength(1)
    expect(wrapper.get('article h2').text()).toBe('Platform Overview')
    wrapper.get('.docs-reader').element.scrollTop = 300
    await wrapper.findAll('.docs-rail-btn')[1].trigger('click')
    await flushPromises()
    expect(wrapper.get('article h2').text()).toBe('Access, Authentication, and Session Lifecycle')
    expect(document.activeElement).toBe(wrapper.get('article h2').element)
    expect(wrapper.get('.docs-reader').element.scrollTop).toBe(0)
    expect(wrapper.get('[aria-current="page"]').text()).toBe('Authentication & sessions')
    await wrapper.findAll('.docs-rail-btn')[0].trigger('click')
    expect(wrapper.get('article h2').text()).toBe('Platform Overview')
  })

  it('filters through the shared search, handles no results, and resets', async () => {
    await openDocs()
    const search = headerSearchFor('docs')
    search.value.query = 'websocket'
    await flushPromises()
    expect(wrapper.find('article').exists()).toBe(true)
    expect(wrapper.get('article').text().toLowerCase()).toContain('websocket')
    search.value.query = 'no-such-doc-xyz'
    await flushPromises()
    expect(wrapper.find('article').exists()).toBe(false)
    expect(wrapper.text()).toContain('No matching sections')
    await wrapper.get('.empty-state button').trigger('click')
    expect(wrapper.get('article h2').text()).toBe('Platform Overview')
  })

  it('opens a topic from the compact selector', async () => {
    await openDocs()
    await wrapper.get('#docs-topic').setValue('navigation-shell')
    await flushPromises()
    expect(wrapper.get('article h2').text()).toBe('Navigation Shell and Layout')
    expect(wrapper.get('pre').text()).toContain('const PAGES')
  })
})
