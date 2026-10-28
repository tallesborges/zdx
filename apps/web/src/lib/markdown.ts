/**
 * Markdown rendering for assistant messages and Markdown artifacts.
 *
 * Both are untrusted, and marked passes raw HTML through, so the result is
 * parsed in an inert document and sanitized against a tag/attribute
 * allowlist before it reaches {@html}.
 */
import { marked } from "marked";

marked.setOptions({ gfm: true, breaks: true });

const ALLOWED_TAGS = new Set([
  "p", "br", "hr", "strong", "em", "del", "code", "pre", "blockquote",
  "ul", "ol", "li", "a", "h1", "h2", "h3", "h4", "h5", "h6",
  "table", "thead", "tbody", "tr", "th", "td", "span",
]);

/** Removed with their content: unwrapping would show code or CSS as text. */
const DROPPED_TAGS = new Set([
  "script", "style", "template", "noscript", "iframe", "object", "embed", "title", "head",
]);

const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title"]),
  code: new Set(["class"]),
  span: new Set(["class"]),
};

function sanitize(root: Element): void {
  for (const el of Array.from(root.querySelectorAll("*"))) {
    const tag = el.tagName.toLowerCase();

    if (DROPPED_TAGS.has(tag)) {
      el.remove();
      continue;
    }

    if (!ALLOWED_TAGS.has(tag)) {
      el.replaceWith(...Array.from(el.childNodes));
      continue;
    }

    const allowed = ALLOWED_ATTRS[tag];
    for (const attr of Array.from(el.attributes)) {
      if (!allowed?.has(attr.name)) {
        el.removeAttribute(attr.name);
        continue;
      }
      // Block javascript:/data: URL vectors on links.
      if (attr.name === "href" && !/^(https?:|mailto:|#|\/)/i.test(attr.value.trim())) {
        el.removeAttribute("href");
      }
    }

    if (tag === "a") {
      el.setAttribute("target", "_blank");
      el.setAttribute("rel", "noopener noreferrer");
    }
  }
}

export function renderMarkdown(source: string): string {
  const raw = marked.parse(source, { async: false });
  // A DOMParser document is inert: no scripts run and no images load. Parsing
  // into an element of the live document would start loads (and fire their
  // `onerror` handlers) before the sanitizer could remove them.
  const body = new DOMParser().parseFromString(raw, "text/html").body;
  sanitize(body);
  return body.innerHTML;
}
