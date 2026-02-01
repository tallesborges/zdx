export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(path)

  if (!res.ok) {
    throw new ApiError(res.status, (await readErrorMessage(res)))
  }

  return (await res.json()) as T
}

async function readErrorMessage(res: Response) {
  const raw = await res.text()
  let message = raw || res.statusText || `HTTP ${res.status}`

  try {
    const data = JSON.parse(raw)
    if (data?.message) {
      message = data.message
    }
  } catch { }

  return message
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
