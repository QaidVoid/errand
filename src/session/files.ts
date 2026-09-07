/**
 * Reading files in a session's project, without involving the agent.
 *
 * Looking at a directory should not cost a model call or wait behind the
 * admission queue. These run in the daemon and answer immediately.
 *
 * Nothing here executes a shell. Each operation is a filesystem call on a path
 * the caller has already confined to the project, so there is no command for a
 * crafted argument to inject into. Reading is structured only: what a thread
 * or an interface makes of it is rendering, and lives with the rendering.
 */

import { extname, join } from "@std/path";

/** Longest file read inline before it is cut. Whole files go by upload. */
export const MAX_INLINE_BYTES = 12 * 1024;

/** Maps a file extension to a fence language, for readable output. */
const LANGUAGES: Record<string, string> = {
  ".ts": "ts",
  ".tsx": "tsx",
  ".js": "js",
  ".jsx": "jsx",
  ".json": "json",
  ".md": "md",
  ".sh": "bash",
  ".bash": "bash",
  ".fish": "fish",
  ".py": "python",
  ".rs": "rust",
  ".go": "go",
  ".c": "c",
  ".h": "c",
  ".cpp": "cpp",
  ".toml": "toml",
  ".yaml": "yaml",
  ".yml": "yaml",
  ".sql": "sql",
  ".html": "html",
  ".css": "css",
};

/** The fence language for a path, or an empty string when unknown. */
export function languageFor(path: string): string {
  return LANGUAGES[extname(path).toLowerCase()] ?? "";
}

/**
 * Whether content looks binary.
 *
 * A NUL byte in the first block is the same heuristic `grep` uses, and it is
 * what stops an image being pasted into a thread as mojibake.
 */
export function looksBinary(bytes: Uint8Array): boolean {
  return bytes.subarray(0, 8_000).includes(0);
}

/** One entry in a directory. */
export interface Entry {
  name: string;
  /** Path relative to the project, so a surface can ask for it again. */
  path: string;
  directory: boolean;
  /** Zero for a directory, and for a file whose size could not be read. */
  size: number;
}

/**
 * Lists a directory, directories first and then by name.
 *
 * One order for every surface: a thread and an interface listing the same
 * directory differently is a bug report waiting to happen.
 */
export function readDirectory(hostPath: string, relative: string): Entry[] {
  const entries: Entry[] = [];
  for (const found of Deno.readDirSync(hostPath)) {
    let size = 0;
    if (!found.isDirectory) {
      try {
        size = Deno.statSync(join(hostPath, found.name)).size;
      } catch {
        size = 0;
      }
    }
    entries.push({
      name: found.name,
      path: relative.length === 0 ? found.name : `${relative}/${found.name}`,
      directory: found.isDirectory,
      size,
    });
  }

  return entries.sort((left, right) =>
    left.directory === right.directory
      ? left.name.localeCompare(right.name)
      : (left.directory ? -1 : 1)
  );
}

/** A file's contents, or a statement that it is not text. */
export interface FileContents {
  path: string;
  size: number;
  binary: boolean;
  /** Whether the file continues past what was read. */
  truncated: boolean;
  text: string;
  /**
   * The language to read it as, or empty when the extension is not one this
   * knows. Reported so a surface highlights what the daemon named rather than
   * deriving a second mapping or guessing from the contents.
   */
  language: string;
}

/** Raised when a path names something that cannot be shown as a file. */
export class NotAFileError extends Error {
  constructor(path: string) {
    super(`${path} is not a file`);
    this.name = "NotAFileError";
  }
}

/**
 * Reads a file for display, refusing to render something that is not text.
 *
 * Only the first `limit` bytes are read: showing a large file inline is not
 * useful, and reading all of it to throw most away costs the whole file.
 *
 * @throws NotAFileError when the path is a directory or a device.
 */
export function readFileForDisplay(
  hostPath: string,
  relative: string,
  limit: number = MAX_INLINE_BYTES,
): FileContents {
  const stat = Deno.statSync(hostPath);
  if (!stat.isFile) throw new NotAFileError(relative);

  const raw = readHead(hostPath, limit);
  const truncated = stat.size > limit;

  if (looksBinary(raw)) {
    return { path: relative, size: stat.size, binary: true, truncated, text: "", language: "" };
  }

  return {
    path: relative,
    size: stat.size,
    binary: false,
    truncated,
    text: decodeWhole(raw, truncated),
    language: languageFor(relative),
  };
}

function readHead(hostPath: string, limit: number): Uint8Array {
  const file = Deno.openSync(hostPath, { read: true });
  try {
    const buffer = new Uint8Array(limit);
    const read = file.readSync(buffer) ?? 0;
    return buffer.subarray(0, read);
  } finally {
    file.close();
  }
}

/**
 * Decodes bytes, dropping a character the cut landed in the middle of.
 *
 * A limit counted in bytes can fall inside a multi-byte character, which
 * decodes to a replacement mark. Showing one at the very end of a file that
 * was cut anyway is noise, so it goes.
 */
function decodeWhole(bytes: Uint8Array, truncated: boolean): string {
  const text = new TextDecoder().decode(bytes);
  return truncated && text.endsWith("\uFFFD") ? text.slice(0, -1) : text;
}
