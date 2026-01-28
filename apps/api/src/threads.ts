import type { ThreadSummary, ThreadDetail } from './types';
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
  const filePath = join(THREADS_DIR, id)


  return null
}
