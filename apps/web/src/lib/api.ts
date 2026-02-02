export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(path)

  if (!res.ok) {
    throw new ApiError(res.status, (await readErrorMessage(res)))
  }

  let response = await res.json()

  if (path === '/api/threads' && !isThreadSummaryArray(response)) {
    throw new ApiError(500, 'Invalid response body')
  }

  return response as T
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

function isThreadSummaryArray(data: unknown): data is ThreadSummary[] {
  const isArray = Array.isArray(data)

  if (!isArray) return false

  for (const item of data) {
    if (typeof item !== 'object' || item === null) {
      return false
    }

    const obj = item as any

    const validId = 'id' in item && typeof obj.id === 'string'
    const validTitle = 'title' in item && typeof obj.title === 'string'
    const validaUpdateAt = 'updatedAt' in item && typeof obj.updatedAt === 'string'

    if (!validId || !validTitle || !validaUpdateAt) {
      return false
    }
  }

  return true
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
