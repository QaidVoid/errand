// deno-lint-ignore-file no-console -- a command line check reports to stdout.
/**
 * Formats and lints what the repository holds, and nothing else.
 *
 * `deno fmt` and `deno lint` walk the checkout, so a directory some tool left
 * behind is read as the project's own and fails the gate over a file nobody
 * committed. Passing the tracked files instead is not the answer: an explicit
 * path overrides the `exclude` in the configuration, so the generated pages and
 * the interface would be checked against rules they are deliberately outside.
 *
 * So the walk stays, and what it must not enter is named: the configuration's
 * own exclusions, plus whatever is in the checkout but not in the repository.
 *
 * Usage:
 *   deno run -A scripts/check_tree.ts check   # refuse on a difference
 *   deno run -A scripts/check_tree.ts fix     # write the difference out
 */

import { untrackedRoots } from "./tracked.ts";

interface DenoConfig {
  exclude?: string[];
  fmt?: { exclude?: string[] };
  lint?: { exclude?: string[] };
}

const config: DenoConfig = JSON.parse(await Deno.readTextFile("deno.json"));

/** What one tool must not enter: its own exclusions, then the strays. */
function ignoresFor(own: string[] | undefined, strays: string[]): string {
  return [...(config.exclude ?? []), ...(own ?? []), ...strays].join(",");
}

async function run(args: string[]): Promise<boolean> {
  const { success } = await new Deno.Command(Deno.execPath(), {
    args,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  return success;
}

const mode = Deno.args[0] ?? "check";
if (mode !== "check" && mode !== "fix") {
  console.error(`usage: check_tree.ts [check|fix], not ${mode}`);
  Deno.exit(2);
}

const strays = await untrackedRoots();
if (strays.length > 0) {
  console.log(`leaving alone what the repository does not hold: ${strays.join(", ")}`);
}

const formatted = await run([
  "fmt",
  ...(mode === "check" ? ["--check"] : []),
  `--ignore=${ignoresFor(config.fmt?.exclude, strays)}`,
]);
const linted = await run(["lint", `--ignore=${ignoresFor(config.lint?.exclude, strays)}`]);

Deno.exit(formatted && linted ? 0 : 1);
