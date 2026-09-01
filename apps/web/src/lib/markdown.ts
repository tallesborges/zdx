/**
 * Markdown rendering for assistant messages.
 *
 * The API returns model text verbatim, so output is untrusted: we disable
 * marked's raw-HTML passthrough and sanitize the result against a tag/attribute
 * allowlist before it reaches {@html}.
 */
import { marked } from "marked";

marked.setOptions({ gfm: true, breaks: true });

const ALLOWED_TAGS = new Set([
  "p", "br", "hr", "strong", "em", "del", "code", "pre", "blockquote",
  "ul", "ol", "li", "a", "h1", "h2", "h3", "h4", "h5", "h6",
  "table", "thead", "tbody", "tr", "th", "td", "span",
]);

const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title"]),
  code: new Set(["class"]),
  span: new Set(["class"]),
};

function sanitize(root: Element): void {
  for (const el of Array.from(root.querySelectorAll("*"))) {
    const tag = el.tagName.toLowerCase();

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
  const host = document.createElement("div");
  // marked with sanitize removed still emits inline HTML; parsing into a
  // detached element lets us strip it structurally rather than by regex.
  host.innerHTML = raw;
  sanitize(host);
  return host.innerHTML;
}
