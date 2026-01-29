# Note: Ratatui Buffer Caching

**Status:** Future consideration  
**Created:** 2025-01-29

---

## TL;DR

Cache ratatui `Buffer` for overlays/tabs that don't change every frame. Store buffer + dirty flag, blit to frame each render, re-render only when content or size changes.

## When useful

- Overlay content changes rarely (command palette hidden, static modals)
- Expensive formatting (markdown, syntax highlighting, large lists)
- Avoiding redraw of complex subtrees

## When not useful

- Content changes every frame (cursor blink, streaming tokens inside overlay)

## Key pieces

- `CachedLayer`: holds `Buffer`, `Rect`, `dirty: bool`
- Invalidate on content change or resize
- `copy_from_buffer()` to blit cached buffer to frame
