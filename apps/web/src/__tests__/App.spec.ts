import { describe, it, expect } from 'vitest'

import { mount } from '@vue/test-utils'
import App from '../App.vue'

describe('App', () => {
  it('mounts renders properly', () => {
    const wrapper = mount(App, {
      global: {
        stubs: { 'router-view': true },
      },
    })
    expect(wrapper.find('main').exists()).toBe(true)
  })
})
