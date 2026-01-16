export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(path)

  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`)
  }

  return (await res.json()) as T
}

export type Health = { ok: boolean }
export type ThreadSummary = { id: string; title: string; updatedAt: string }
export type ThreadDetail = {
  id: string
  title: string
  messages: { role: 'user' | 'assistant'; content: string }[]
}
