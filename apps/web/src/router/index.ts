import { createRouter, createWebHistory } from 'vue-router'

import ThreadDetail from '@/pages/ThreadDetail.vue'
import ThreadsList from '@/pages/ThreadsList.vue'
import Home from '@/pages/Home.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: '/', name: 'home', component: Home },
    { path: '/threads', name: 'threads', component: ThreadsList },
    { path: '/threads/:id', name: 'thread-detail', component: ThreadDetail },
  ],
})

export default router
