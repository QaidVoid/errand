// deno-lint-ignore-file no-console -- a command line check reports to stdout.
/**
 * Rejects non-ASCII characters in tracked text files.
 *
 * Chat output is allowed emoji, but only from the one file that enumerates
 * them. Every other tracked file stays pure ASCII so that logs, diffs, and
 * commit messages never depend on terminal font coverage.
 */

const ALLOWED = new Set(["src/discord/chars.ts"]);

const BINARY = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".woff", ".woff2"];

interface Offence {
  file: string;
  line: number;
  column: number;
  character: string;
}

async function tracked(): Promise<string[]> {
  const listed = new Deno.Command("jj", {
    args: ["file", "list"],
    stdout: "piped",
    stderr: "null",
  });
  const { code, stdout } = await listed.output();
  if (code !== 0) throw new Error("could not list tracked files; is this a jj repository?");
  return new TextDecoder()
    .decode(stdout)
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function scan(file: string, text: string): Offence[] {
  const offences: Offence[] = [];
  text.split("\n").forEach((line, index) => {
    for (let column = 0; column < line.length; column += 1) {
      const character = line[column] as string;
      if (character.charCodeAt(0) > 127) {
        offences.push({ file, line: index + 1, column: column + 1, character });
      }
    }
  });
  return offences;
}

const files = await tracked();
const offences: Offence[] = [];
let checked = 0;

for (const file of files) {
  if (ALLOWED.has(file)) continue;
  if (BINARY.some((extension) => file.endsWith(extension))) continue;
  let text: string;
  try {
    text = await Deno.readTextFile(file);
  } catch {
    continue;
  }
  checked += 1;
  offences.push(...scan(file, text));
}

if (offences.length > 0) {
  for (const offence of offences) {
    const code = offence.character.codePointAt(0)?.toString(16) ?? "?";
    console.error(`${offence.file}:${offence.line}:${offence.column} non-ascii U+${code}`);
  }
  console.error(`${offences.length} non-ascii character(s) in tracked files`);
  Deno.exit(1);
}

console.log(`ascii check passed across ${checked} tracked files`);
