// deno-lint-ignore-file no-console -- a command line check reports to stdout.
/**
 * Rejects non-ASCII characters in tracked text files.
 *
 * No file is exempt. Chat output may carry emoji, but the table that
 * enumerates them declares codepoints rather than glyphs, so even that file is
 * ASCII. Everything stays greppable in a terminal with no font coverage.
 */

const BINARY = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".woff", ".woff2"];

interface Offence {
  file: string;
  line: number;
  column: number;
  character: string;
}

/**
 * Asks one tool for the tracked files, or nothing when it is not installed.
 *
 * A tool that is absent is not a failure: the checkout this runs in decides
 * which one is there, and continuous integration has only git.
 */
async function listedBy(program: string, args: string[]): Promise<string[] | undefined> {
  let output;
  try {
    output = await new Deno.Command(program, { args, stdout: "piped", stderr: "null" }).output();
  } catch {
    return undefined;
  }
  if (!output.success) return undefined;
  return new TextDecoder()
    .decode(output.stdout)
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

async function tracked(): Promise<string[]> {
  const files = await listedBy("jj", ["file", "list"]) ??
    await listedBy("git", ["ls-files"]);
  if (files === undefined) {
    throw new Error("could not list tracked files; neither jj nor git answered");
  }
  return files;
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
