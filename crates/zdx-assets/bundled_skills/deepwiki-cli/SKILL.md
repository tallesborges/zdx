---
name: deepwiki-cli
description: Use DeepWiki CLI to query GitHub repositories for implementation details, architecture, and documentation. Trigger when user asks about a GitHub repo's code, how something is implemented in a repo, repo architecture, or wants to read repo documentation. Use parallel-cli for general web search or arbitrary URLs instead.
---

# DeepWiki CLI

Query GitHub repositories for implementation details, architecture, and documentation.

## Commands

### ask-question

Ask a specific question about a repository:

```bash
deepwiki ask-question --repo-name <owner/repo> --question "<question>"
```

### read-wiki-structure

Get table of contents:

```bash
deepwiki read-wiki-structure --repo-name <owner/repo>
```

### read-wiki-contents

Get full documentation with code examples:

```bash
deepwiki read-wiki-contents --repo-name <owner/repo>
```

## Required Options

| Option | Description |
|--------|-------------|
| `--repo-name <repo>` | Format: `owner/repo` (e.g., `facebook/react`) |
| `--question <text>` | **Required for `ask-question`** |

## Optional Options

| Option | Description |
|--------|-------------|
| `-o, --output <format>` | `text` \| `markdown` \| `json` \| `raw` (default: text) |
| `-t, --timeout <ms>` | Timeout in milliseconds |

## Examples

```bash
deepwiki ask-question --repo-name facebook/react --question "How do hooks work?"
deepwiki ask-question --repo-name vercel/next.js --question "How is routing implemented?"
deepwiki read-wiki-structure --repo-name vercel/next.js
deepwiki read-wiki-contents --repo-name golang/go -o json
```
