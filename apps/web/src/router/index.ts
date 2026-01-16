import { createRouter, createWebHistory } from 'vue-router'

import ThreadDetail from '../pages/ThreadDetail.vue'
import ThreadsList from '../pages/ThreadsList.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: '/threads', name: 'threads', component: ThreadsList },
    { path: '/threads/:id', name: 'thread-detail', component: ThreadDetail },
  ],
})

export default router
