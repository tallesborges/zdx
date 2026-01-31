export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(path)

  if (!res.ok) {
    let message = (await res.json()).message ?? res.statusText
    throw new ApiError(res.status, message)
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

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message)
    this.name = 'ApiError'
  }
}
