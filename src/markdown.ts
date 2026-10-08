import { marked } from "marked";
import DOMPurify from "dompurify";

export function renderMarkdown(body: string): string {
  const html = marked.parse(body, { async: false, gfm: true });
  return DOMPurify.sanitize(html, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["form", "input", "button", "select", "textarea", "style"],
    FORBID_ATTR: ["style", "id", "name"],
  });
}

export function externalMarkdownUrl(href: string): string | null {
  try {
    const url = new URL(href);
    return ["https:", "http:", "mailto:"].includes(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}
