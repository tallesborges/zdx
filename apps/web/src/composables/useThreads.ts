import { ref } from 'vue'
import { apiGet } from '@/lib/api'
import type { ThreadSummary } from '@/lib/api'

export function useThreads() {
  const threads = ref<ThreadSummary[] | null>(null)
  const error = ref<string | null>(null)
  const loading = ref(false)

  async function loadThreads() {
    try {
      loading.value = true
      error.value = null
      threads.value = sortThreads(await apiGet<ThreadSummary[]>('/api/threads'))
    } catch (err) {
      error.value = (err as Error).message
    } finally {
      loading.value = false
    }
  }

  return { threads, error, loading, loadThreads }
}

function sortThreads(threads: ThreadSummary[]) {
  return [...threads].sort((a, b) => {
    if (a.updatedAt && b.updatedAt) {
      return b.updatedAt.localeCompare(a.updatedAt)
    }

    if (a.updatedAt) {
      return -1
    }

    return 1
  })
}
