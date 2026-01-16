export type ThreadSummary = {id : string, title: string, updatedAt: string};
export type ThreadDetail = {id: string, title: string, messages: {role: 'user' | 'assistant', content : string}[]};
