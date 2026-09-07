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
import { EnforcementGapError } from "./daemon.ts";
import { AlreadyRunningError } from "./lock.ts";
import { createLogger } from "./log.ts";
import { SandboxUnavailableError } from "./sandbox/backend.ts";
import { serve } from "./serve.ts";
import { treeBytes } from "./session/disk.ts";
import { ThreadRegistry } from "./session/registry.ts";

const USAGE = [
  "usage: errand <command>",
  "",
  "  run                  run the daemon until it is told to stop",
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

/**
 * Runs the daemon, turning the failures an operator can act on into an exit
 * code and one line rather than a stack trace.
 */
async function run(): Promise<number> {
  const log = createLogger();
  try {
    return await serve(loadConfig(configPath(Deno.env.toObject())), log);
  } catch (error) {
    if (error instanceof AlreadyRunningError) {
      log.error(error.message);
      return 4;
    }
    if (error instanceof EnforcementGapError) {
      log.error(error.message);
      return 3;
    }
    if (error instanceof SandboxUnavailableError) {
      log.error(error.message);
      return 2;
    }

    const detail = String(error);
    if (detail.includes("TokenInvalid")) {
      log.error(
        "the chat service rejected the bot token; set chat.token in the configuration file",
      );
      return 2;
    }
    if (detail.includes("DisallowedIntents")) {
      log.error(
        "the chat service refused the gateway intents; enable the Message Content intent for this bot in its developer portal, under Bot, Privileged Gateway Intents",
      );
      return 2;
    }
    log.error("the daemon failed to start", { detail });
    return 1;
  }
}

async function main(args: readonly string[]): Promise<number> {
  const [command, ...rest] = args;

  try {
    switch (command) {
      case "run":
        return await run();
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
