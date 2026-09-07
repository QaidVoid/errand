// deno-lint-ignore-file no-console -- the entry point talks to a terminal.
/**
 * The command line: what an operator runs.
 *
 * The daemon is one subcommand among several rather than the only thing this
 * program does, because managing what the daemon left on disk is an operator's
 * job and belongs where an operator already is.
 */

import { runThreads } from "./cli/threads.ts";
import { configPath, loadConfig } from "./config/load.ts";
import { ConfigError } from "./config/schema.ts";
import { createLogger } from "./log.ts";
import { treeBytes } from "./session/disk.ts";
import { ThreadRegistry } from "./session/registry.ts";

const USAGE = [
  "usage: errand <command>",
  "",
  "  threads [command]    manage remembered threads and their data",
  "  help                 this",
  "",
  `the configuration is read from ${configPath(Deno.env.toObject())}`,
].join("\n");

async function threads(args: readonly string[]): Promise<number> {
  const config = loadConfig(configPath(Deno.env.toObject()));
  const log = createLogger({});
  const registry = new ThreadRegistry(ThreadRegistry.pathFor(config.stateDir), log);
  registry.load();

  return await runThreads(args, {
    registry,
    sizeOf: (stateDir) => treeBytes(stateDir),
    remove: (stateDir) => Deno.remove(stateDir, { recursive: true }),
    write: (line) => console.log(line),
    now: () => Date.now(),
  });
}

async function main(args: readonly string[]): Promise<number> {
  const [command, ...rest] = args;

  try {
    switch (command) {
      case "threads":
        return await threads(rest);
      case "help":
      case "--help":
      case undefined:
        console.log(USAGE);
        return command === undefined ? 2 : 0;
      default:
        console.error(`there is no command called ${command}`);
        console.error(USAGE);
        return 2;
    }
  } catch (error) {
    // A configuration problem is the operator's to fix, so it is printed as
    // itself rather than as a stack trace.
    if (error instanceof ConfigError) {
      console.error(String(error));
      return 1;
    }
    throw error;
  }
}

if (import.meta.main) {
  Deno.exit(await main(Deno.args));
}
