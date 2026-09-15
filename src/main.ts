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
import { basename, join } from "@std/path";
import { POLICY_FILENAME } from "./sandbox/policy.ts";
import { treeBytes } from "./session/disk.ts";
import { recordDir } from "./session/record.ts";
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

/**
 * The project a session worked in, read back from the policy it was run under.
 *
 * The policy names the project as the grant placed at the workspace, which is
 * the one thing on disk that still says where the work was. Nothing is guessed
 * at: a policy that does not say returns nothing, and the caller refuses.
 */
async function projectOf(
  stateDir: string,
): Promise<{ name: string; path: string } | undefined> {
  let policy: string;
  try {
    policy = await Deno.readTextFile(join(stateDir, POLICY_FILENAME));
  } catch {
    return undefined;
  }
  const placed = /\{\s*path\s*=\s*"([^"]+)"\s*,\s*at\s*=\s*"\/workspace"\s*\}/.exec(policy);
  const path = placed?.[1];
  if (path === undefined) return undefined;
  return { name: basename(path), path };
}

async function threads(args: readonly string[]): Promise<number> {
  const config = loadConfig(configPath(Deno.env.toObject()));
  const log = createLogger({});
  const registry = new ThreadRegistry(ThreadRegistry.pathFor(config.stateDir), log);
  registry.load();

  return await runThreads(args, {
    registry,
    stateRoot: config.stateDir,
    projectOf: (stateDir) => projectOf(stateDir),
    sizeOf: (stateDir) => treeBytes(stateDir),
    remove: async (stateDir) => {
      await Deno.remove(stateDir, { recursive: true });
      // The record sits beside the state directory, so removing a thread has
      // to take it too or the transcript outlives what it describes.
      await Deno.remove(recordDir(stateDir), { recursive: true }).catch(() => {});
    },
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
