/**
 * What the repository actually holds, as the version control system sees it.
 *
 * A checkout accumulates directories that belong to whatever tools somebody
 * runs in it, and those are not the project's to format, lint, or hold to its
 * character set. Asking the version control system which files are tracked
 * answers that without naming any tool: a file nobody committed is not ours.
 *
 * Naming them in an ignore file would work too, and would publish which tools
 * the author happens to use. This does not.
 */

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

/** Every tracked file, by path relative to the repository root. */
export async function trackedFiles(): Promise<string[]> {
  const files = await listedBy("jj", ["file", "list"]) ?? await listedBy("git", ["ls-files"]);
  if (files === undefined) {
    throw new Error("could not list tracked files; neither jj nor git answered");
  }
  return files;
}

/**
 * Top level entries that are in the checkout but not in the repository.
 *
 * Top level because that is where a tool puts its directory, and because an
 * ignore list of every stray file would be longer than the tree. Anything
 * deeper that nobody committed sits under a directory that is either tracked,
 * and so the project's own, or already named here.
 */
export async function untrackedRoots(): Promise<string[]> {
  const tracked = new Set((await trackedFiles()).map((path) => path.split("/")[0]));
  const roots: string[] = [];
  for await (const entry of Deno.readDir(".")) {
    // The version control directories are nobody's to check and are never
    // listed as tracked, so they would otherwise be named on every run.
    if (entry.name === ".git" || entry.name === ".jj") continue;
    if (!tracked.has(entry.name)) roots.push(entry.name);
  }
  return roots.sort();
}
