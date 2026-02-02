import { Hono } from 'hono'

import { getThreadDetail, listThreads } from './threads';

const app = new Hono()

app.get('/', (c) => {
  return c.text('Hello Hono!')
})

app.get('/health', (c) => {
  return c.json({ 'ok': true })
})

app.get('/threads', async (c) => {
  const threads = await listThreads()
  return c.json(threads);
});

app.get('/threads/:id', async (c) => {
  const id = c.req.param('id');

  const detail = await getThreadDetail(id)

  if (detail === null) return c.json({ message: "Thread not found" }, 404)

  return c.json(detail);
})

app.get('/threads/:id/messages/:index', async (c) => {
  const id = c.req.param('id')
  const detail = await getThreadDetail(id)

  if (detail === null) return c.json({ message: "Thread not found" }, 404)

  const index = Number(c.req.param('index'))

  if (!Number.isInteger(index)) {
    return c.json({ message: "Invalid index" }, 400)
  } else if (index < 0 || index >= detail.messages.length) {
    return c.json({ message: "Message not found" }, 404)
  }

  return c.json(detail.messages[index])
})

export default app
