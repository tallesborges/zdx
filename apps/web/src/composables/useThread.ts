import { ApiError, apiGet, type ThreadDetail } from "@/lib/api";
import { ref } from "vue";

export function useThread() {
  const thread = ref<ThreadDetail | null>(null)
  const error = ref<string | null>(null)
  const notFound = ref<boolean>(false)
  const loading = ref(false)

  async function loadThread(id: unknown) {
    id = getParamId(id)

    try {
      error.value = null
      notFound.value = false
      loading.value = true
      thread.value = null

      if (id === null) {
        error.value = "Invalid thread id"
        return
      }

      thread.value = await apiGet<ThreadDetail>(`/api/threads/${id}`)
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        notFound.value = true
      } else {
        error.value = (err as Error).message
      }
    } finally {
      loading.value = false
    }
  }

  return { thread, error, notFound, loading, getParamId, loadThread }
}

function getParamId(val: unknown): string | null {
  if (typeof val === 'string') return val

  if (Array.isArray(val) && val.length > 0 && typeof val[0] === 'string') {
    return val[0]
  }

  return null
}
