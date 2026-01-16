import { describe, it, expect, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import { ref } from 'vue'

vi.mock('@/composables/useThreads', () => ({
  useThreads: () => ({
    threads: ref([]),
    error: ref(null),
    loading: ref(true),
    loadThreads: vi.fn(),
  }),
}))

describe('ThreadsList', () => {
  it('shows loading state', async () => {
    const { default: ThreadsList } = await import('@/pages/ThreadsList.vue')
    const wrapper = mount(ThreadsList, {
      global: {
        stubs: { 'router-link': true },
      },
    })
    expect(wrapper.text()).toContain('Loading')
  })
})
