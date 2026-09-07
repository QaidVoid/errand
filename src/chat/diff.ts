/**
 * Renders what an edit changed, as a unified diff.
 *
 * A diff rather than the file: it shows intent instead of contents, it is a
 * fraction of the size, and it does not push a whole file into a chat every
 * time one line moves. The whole file is still available on request.
 *
 * Implemented here rather than by shelling out to `diff`, because a subprocess
 * per edit is a lot of machinery for something this small, and this keeps the
 * output format ours to bound.
 */

/** Lines of context kept either side of a change. */
export const CONTEXT_LINES = 2;

/** Most diff lines posted before the rest is summarised. */
export const MAX_DIFF_LINES = 60;

/** Longest single line shown before it is cut, since a minified file is one line. */
const MAX_LINE_LENGTH = 200;

/** A change to one file, ready to post. */
export interface FileDiff {
  /** True when there is nothing to show. */
  empty: boolean;
  /** Lines added across the whole file. */
  added: number;
  /** Lines removed across the whole file. */
  removed: number;
  /** The rendered diff body, already bounded. */
  body: string;
}

/**
 * Longest common subsequence of two line arrays, as a table of match lengths.
 *
 * Quadratic, which is fine for a source file and is bounded by the caller
 * refusing to diff anything large.
 */
function lcsTable(a: readonly string[], b: readonly string[]): Uint32Array {
  const width = b.length + 1;
  const table = new Uint32Array((a.length + 1) * width);

  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      table[i * width + j] = a[i] === b[j]
        ? (table[(i + 1) * width + j + 1] ?? 0) + 1
        : Math.max(table[(i + 1) * width + j] ?? 0, table[i * width + j + 1] ?? 0);
    }
  }
  return table;
}

/** One line of a diff: kept, added, or removed. */
interface Op {
  kind: " " | "+" | "-";
  text: string;
}

function operations(a: readonly string[], b: readonly string[]): Op[] {
  const width = b.length + 1;
  const table = lcsTable(a, b);
  const ops: Op[] = [];

  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      ops.push({ kind: " ", text: a[i] as string });
      i += 1;
      j += 1;
    } else if ((table[(i + 1) * width + j] ?? 0) >= (table[i * width + j + 1] ?? 0)) {
      ops.push({ kind: "-", text: a[i] as string });
      i += 1;
    } else {
      ops.push({ kind: "+", text: b[j] as string });
      j += 1;
    }
  }
  for (; i < a.length; i += 1) ops.push({ kind: "-", text: a[i] as string });
  for (; j < b.length; j += 1) ops.push({ kind: "+", text: b[j] as string });

  return ops;
}

function clip(text: string): string {
  const points = Array.from(text);
  return points.length > MAX_LINE_LENGTH
    ? `${points.slice(0, MAX_LINE_LENGTH).join("")} ...`
    : text;
}

/**
 * Builds a bounded unified diff between two versions of a file.
 *
 * Unchanged regions are dropped apart from a little context, so a one line
 * change in a thousand line file reads as a one line change.
 */
export function fileDiff(before: string, after: string): FileDiff {
  if (before === after) return { empty: true, added: 0, removed: 0, body: "" };

  const ops = operations(before.split("\n"), after.split("\n"));
  const added = ops.filter((op) => op.kind === "+").length;
  const removed = ops.filter((op) => op.kind === "-").length;

  // Keep a changed line, and any context line close enough to one.
  const keep = new Array<boolean>(ops.length).fill(false);
  ops.forEach((op, index) => {
    if (op.kind === " ") return;
    const from = Math.max(0, index - CONTEXT_LINES);
    const to = Math.min(ops.length - 1, index + CONTEXT_LINES);
    for (let k = from; k <= to; k += 1) keep[k] = true;
  });

  const lines: string[] = [];
  let truncated = 0;
  let previousKept = -1;

  for (let index = 0; index < ops.length; index += 1) {
    if (!keep[index]) continue;
    if (lines.length >= MAX_DIFF_LINES) {
      truncated += 1;
      continue;
    }
    if (previousKept !== -1 && index > previousKept + 1) lines.push("@@");
    const op = ops[index] as Op;
    lines.push(`${op.kind}${clip(op.text)}`);
    previousKept = index;
  }

  if (truncated > 0) lines.push(`@@ ${truncated} further line(s) not shown`);

  return { empty: false, added, removed, body: lines.join("\n") };
}

/** The message posted for a change to one file. */
export function renderDiff(path: string, diff: FileDiff): string {
  const summary = `\`${path}\` +${diff.added} -${diff.removed}`;
  return `${summary}\n\`\`\`diff\n${diff.body}\n\`\`\``;
}
