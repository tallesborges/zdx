export type ThreadSummary = { id: string, title: string, updatedAt: string };
export type ThreadDetail = { id: string, title: string, messages: ThreadMessage[] };
export type ThreadMessage = { role: 'user' | 'assistant', content: string }
