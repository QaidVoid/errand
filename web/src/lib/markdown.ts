/**
 * Markdown, rendered safely.
 *
 * What a session says is model output and file contents, neither of which is
 * trusted input. Markdown allows raw HTML, so parsing alone would let a file
 * the agent read put script into this page. Everything is therefore sanitised
 * after parsing, and that step is not optional.
 *
 * Highlighting happens before sanitising rather than after, so that what it
 * produces is filtered by the same pass as everything else instead of being
 * trusted because this module generated it.
 */

import DOMPurify from "dompurify";
import { marked } from "marked";
import { highlight } from "./highlight.ts";

// Fenced blocks are highlighted here, where the language on the fence is
// still available. The highlighter escapes what it wraps, so the code shown is
// the code that was written.
marked.use({
  renderer: {
    code({ text, lang }) {
      const language = (lang ?? "").split(/\s+/)[0];
      const marked = highlight(text, language);
      const body = marked ??
        text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
      const named = language === undefined || language.length === 0
        ? ""
        : ` class="language-${language.replace(/[^a-z0-9-]/gi, "")}"`;
      return `<pre><code${named}>${body}</code></pre>\n`;
    },
  },
});

marked.setOptions({
  // A single newline is a line break. Chat output is written expecting that,
  // and paragraph-only breaks read as one run-on paragraph.
  breaks: true,
  gfm: true,
});

/** Tags worth keeping. Anything else is dropped rather than rendered. */
export const ALLOWED_TAGS = [
  "span",
  "p",
  "br",
  "strong",
  "em",
  "del",
  "code",
  "pre",
  "blockquote",
  "ul",
  "ol",
  "li",
  "a",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "table",
  "thead",
  "tbody",
  "tr",
  "th",
  "td",
];

/** Escapes text so it can be shown without being interpreted as markup. */
/**
 * Attributes worth keeping.
 *
 * A class carries a colour and nothing else, which is what makes it safe to
 * allow for highlighting. Everything that can act, and `style`, stay out.
 */
export const ALLOWED_ATTR = ["href", "title", "class"];

function escaped(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * What has already been rendered, by the text it came from.
 *
 * Rendering is parsing, highlighting, and sanitising, and a session redraws
 * whenever anything arrives. Without this, every message in a session is put
 * through all three again on every new entry, which is what makes a long live
 * session crawl. The text of a message never changes once reported, so the
 * result can simply be kept.
 */
const rendered = new Map<string, string>();

/** Bounded so a very long session cannot grow this without limit. */
const MAX_CACHED = 600;

/**
 * Renders markdown to HTML that is safe to insert.
 *
 * Where sanitising is unavailable this shows the text as written rather than
 * rendering it. DOMPurify returns its input untouched when it cannot run, so
 * without this check an environment it does not support would silently turn
 * into no sanitising at all.
 */
export function render(text: string): string {
  if (!DOMPurify.isSupported) return `<p>${escaped(text)}</p>`;

  const seen = rendered.get(text);
  if (seen !== undefined) return seen;

  const parsed = marked.parse(text, { async: false });
  const clean = DOMPurify.sanitize(parsed, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    // Only ordinary links, so a rendered message cannot carry javascript: at
    // all rather than relying on the browser to refuse it.
    ALLOWED_URI_REGEXP: /^(?:https?|mailto):/i,
  });

  // Oldest first, which for a transcript is the part furthest from what is
  // being read now.
  if (rendered.size >= MAX_CACHED) {
    const oldest = rendered.keys().next();
    if (!oldest.done) rendered.delete(oldest.value);
  }
  rendered.set(text, clean);
  return clean;
}
