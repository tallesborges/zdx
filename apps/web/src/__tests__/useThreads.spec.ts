import { useThreads } from "@/composables/useThreads";
import { apiGet, type ThreadSummary } from "@/lib/api";
import { it, describe, vi, expect } from "vitest";

vi.mock('@/lib/api', () => ({ apiGet: vi.fn() }))

describe('useThreads', () => {
  it('loads threads successfully', async () => {
    const api = vi.mocked(apiGet)
    const mockData: ThreadSummary[] = [
      { id: '2', title: "b", updatedAt: '2024-01-01T00:00:00Z' },
      { id: '3', title: "c", updatedAt: '' },
      { id: '1', title: "a", updatedAt: '2024-02-01T00:00:00Z' },
    ]
    const expected: ThreadSummary[] = [
      { id: '1', title: "a", updatedAt: '2024-02-01T00:00:00Z' },
      { id: '2', title: "b", updatedAt: '2024-01-01T00:00:00Z' },
      { id: '3', title: "c", updatedAt: '' },
    ]
    api.mockResolvedValueOnce(mockData)

    const { threads, error, loading, loadThreads } = useThreads()
    await loadThreads()

    expect(loading.value).toBe(false)
    expect(error.value).toBe(null)
    expect(threads.value).toEqual(expected)
  })

  it('handles API error', async () => {
    const api = vi.mocked(apiGet)
    api.mockRejectedValueOnce(new Error('HTTP 500'))

    const { threads, error, loading, loadThreads } = useThreads()
    await loadThreads()

    expect(loading.value).toBe(false)
    expect(error.value).toBe('HTTP 500')
    expect(threads.value).toBe(null)
  })
})
