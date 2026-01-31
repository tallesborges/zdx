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

export default app
