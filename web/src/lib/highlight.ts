/**
 * Syntax highlighting.
 *
 * Only the languages actually read here are registered, rather than the whole
 * bundle, so what ships is what is used.
 *
 * The highlighter emits classes and leaves the colours to the stylesheet, which
 * is what lets highlighted code follow the theme from the same tokens as
 * everything else. It also escapes what it wraps, so the text shown is the text
 * that was given.
 */

import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import ruby from "highlight.js/lib/languages/ruby";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const LANGUAGES: Record<string, unknown> = {
  bash,
  css,
  diff,
  go,
  ini,
  java,
  javascript,
  json,
  markdown,
  python,
  ruby,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};

for (const [name, language] of Object.entries(LANGUAGES)) {
  hljs.registerLanguage(name, language as never);
}

/** Names the daemon or a fence may use, and the language they mean here. */
const ALIASES: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  py: "python",
  rb: "ruby",
  rs: "rust",
  yml: "yaml",
  html: "xml",
  svg: "xml",
  md: "markdown",
  toml: "ini",
  conf: "ini",
  patch: "diff",
};

/**
 * The language to highlight as, or undefined when there is none to use.
 *
 * Nothing is guessed: an unknown name means plain text, because highlighting a
 * short file as the wrong language reads worse than not highlighting it.
 */
export function languageOf(name: string | undefined): string | undefined {
  if (name === undefined) return undefined;
  const lowered = name.trim().toLowerCase();
  if (lowered.length === 0) return undefined;

  const resolved = ALIASES[lowered] ?? lowered;
  return hljs.getLanguage(resolved) === undefined ? undefined : resolved;
}

/**
 * Highlights code, returning markup.
 *
 * @returns undefined when the language is unknown, so the caller shows the text
 *   plainly rather than being handed something it did not ask for.
 */
export function highlight(code: string, name: string | undefined): string | undefined {
  const language = languageOf(name);
  if (language === undefined) return undefined;

  try {
    return hljs.highlight(code, { language, ignoreIllegals: true }).value;
  } catch {
    // A grammar that throws on strange input is not worth failing a page for.
    return undefined;
  }
}
