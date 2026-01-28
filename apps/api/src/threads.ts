import type { ThreadSummary, ThreadDetail, ThreadMessage } from './types';
import { readdir, readFile } from 'node:fs/promises'
import { join } from 'node:path';

const THREADS_DIR = "/Users/tallesborges/.config/zdx/threads"

export async function listThreads(): Promise<ThreadSummary[]> {
  const files = await readdir(THREADS_DIR)
  const summaries: ThreadSummary[] = [];

  for (const filename of files) {
    const fullPath = join(THREADS_DIR, filename)

    if (!filename.endsWith('.jsonl')) {
      continue;
    }
    const file = await readFile(fullPath, 'utf-8')
    const lines = file.split('\n').filter(l => l !== '')

    for (const line of lines) {
      const data = JSON.parse(line);
      if (data.type !== 'meta') continue
      const id = filename.replace(/\.jsonl$/, '')
      summaries.push({ id: id, title: data.title ?? id, updatedAt: data.ts ?? '' })
      break
    }

    summaries.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
  }

  return summaries;
}

export async function getThreadDetail(id: string): Promise<ThreadDetail | null> {
  const filePath = join(THREADS_DIR, `${id}.jsonl`)

  const fileData = await readFile(filePath, 'utf-8').catch(() => null)
  if (!fileData) return null

  const lines = fileData.split('\n').filter(l => l !== '')

  let title = id
  let messages: ThreadMessage[] = []

  for (const l of lines) {
    const data = JSON.parse(l)

    if (data.type === 'meta') {
      title = data.title ?? id
    } else if (data.type === 'message') {
      messages.push({ role: data.role, content: data.text })
    }
  }

  return {
    id,
    title,
    messages
  }
}
