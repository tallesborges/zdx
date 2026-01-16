import { Hono } from 'hono'

import type { ThreadSummary, ThreadDetail } from './types';
import { listThreads } from './threads';

const THREADS : ThreadSummary[] = [
  { id: "1", title: "First", updatedAt: "2026-01-15T12:00:00Z" },
  { id: "2", title: "Second", updatedAt: "2026-01-15T12:00:00Z" },
  { id: "3", title: "Third", updatedAt: "2026-01-15T12:00:00Z" },
];

const app = new Hono()

app.get('/', (c) => {
  return c.text('Hello Hono!')
})

app.get('/health', (c) => {
  return c.json({'ok': true})
})

app.get('/threads', async (c) => {
  const threads = await listThreads()
  return c.json(threads);
});

app.get('/threads/:id', (c) => {
  const id = Number(c.req.param('id'));

  if (Number.isNaN(id)) {
    return c.json({ error: 'Bad id' }, 400);
  }

  const thread = THREADS.find(t => t.id === String(id));
  if (!thread) {
    return c.json({ error: 'Not Found'}, 404);
  }

  const detail: ThreadDetail = {
    ...thread,
    messages: [
      { role: 'user', content: 'Hey' },
      { role: 'assistant', content: 'Hey there, how can I assist you?'}
    ]
  }

  return c.json(detail);
})

export default app
