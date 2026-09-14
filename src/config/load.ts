/**
 * Finding and reading the configuration file.
 *
 * Separate from validating it, so that "the file is not there" and "the file
 * says something impossible" are different failures with different messages.
 *
 * Where it is read from is a search rather than one path, because the same
 * program is run in three ways: from a checkout while it is being worked on,
 * as somebody's own daemon, and as a system service. Naming the file outright
 * always wins, so none of that has to be guessed at when it matters.
 */

import { dirname, join, resolve } from "@std/path";
import { parse as parseJsonc } from "@std/jsonc";
import { type Config, ConfigError } from "./schema.ts";
import { validateConfig } from "./validate.ts";

/** Environment variable naming the configuration file. */
export const CONFIG_VARIABLE = "ERRAND_CONFIG";

/** The directory name used under a configuration root. */
export const CONFIG_DIRECTORY = "errand";

/** The filename, wherever it is found. */
export const CONFIG_FILENAME = "config.json";

/** The same file with the extension that says it may carry comments. */
export const CONFIG_FILENAME_JSONC = "config.jsonc";

/** Both accepted basenames, plain JSON first so an existing setup is unchanged. */
const CONFIG_BASENAMES = [CONFIG_FILENAME, CONFIG_FILENAME_JSONC] as const;

/** Where a system service keeps it. */
export const SYSTEM_CONFIG_PATH = `/etc/${CONFIG_DIRECTORY}/${CONFIG_FILENAME}`;

/**
 * Every place the configuration is looked for, in order.
 *
 * A person's own configuration comes before the system's, so running the
 * daemon by hand on a host that also serves one does not silently pick up the
 * service's token. The working directory is last: it is a convenience for a
 * checkout, not somewhere a daemon should be configured from by accident.
 */
export function configCandidates(env: Record<string, string | undefined>): string[] {
  const named = env[CONFIG_VARIABLE]?.trim();
  if (named !== undefined && named.length > 0) return [named];

  const home = env.HOME?.trim() ?? "";
  const xdg = env.XDG_CONFIG_HOME?.trim();
  const root = xdg !== undefined && xdg.length > 0 ? xdg : join(home, ".config");

  const dirs = [join(root, CONFIG_DIRECTORY), `/etc/${CONFIG_DIRECTORY}`, "."];
  return dirs.flatMap((dir) =>
    CONFIG_BASENAMES.map((name) => (dir === "." ? name : join(dir, name)))
  );
}

/**
 * Where the configuration will be read from.
 *
 * @returns the first candidate that exists, or the first candidate when none
 *   do, so that a failure names the place somebody most likely meant.
 */
export function configPath(
  env: Record<string, string | undefined>,
  exists: (path: string) => boolean = fileExists,
): string {
  const candidates = configCandidates(env);
  return candidates.find(exists) ?? (candidates[0] as string);
}

function fileExists(path: string): boolean {
  try {
    return Deno.statSync(path).isFile;
  } catch {
    return false;
  }
}

/**
 * Reads and validates the configuration.
 *
 * @throws ConfigError with something actionable: where it looked, the place
 *   the file could not be parsed, or every field that was wrong.
 */
export function loadConfig(
  path: string,
  read = Deno.readTextFileSync,
  env: Record<string, string | undefined> = {},
  exists: (path: string) => boolean = fileExists,
): Config {
  let text: string;
  try {
    text = read(path);
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) {
      throw new ConfigError([`the configuration file at ${path} could not be read: ${error}`]);
    }
    // Every place that was tried, since "not at that path" is not much help
    // when the path was chosen by a search somebody did not run themselves.
    const looked = configCandidates(env);
    throw new ConfigError([
      `there is no configuration file at ${path}`,
      ...(looked.length > 1 ? [`looked in: ${looked.join(", ")}`] : []),
      `write one there, or name it with ${CONFIG_VARIABLE}`,
    ]);
  }

  let parsed: unknown;
  try {
    // Parsed as JSONC so a file may carry comments and trailing commas, which
    // is the difference between a config someone can annotate and one they
    // cannot. Plain JSON is a subset, so an existing file still reads.
    parsed = parseJsonc(text);
  } catch (error) {
    throw new ConfigError([
      `the configuration file at ${path} is not valid JSON or JSONC: ${error}`,
    ]);
  }

  const config = validateConfig(parsed);
  return config.agent.rulesPath === undefined ? withRulesBeside(config, path, exists) : config;
}

/** House rules looked for beside the configuration when none were named. */
export const RULES_FILENAME = "AGENTS.md";

/**
 * Takes house rules from beside the configuration file, when none were named.
 *
 * Beside the configuration rather than at a fixed path, so the rules follow
 * whichever of the candidates was actually loaded: an operator with a file in
 * their home directory and another in `/etc` gets the one belonging to the
 * configuration in force, not whichever the search happened to reach first.
 *
 * Only when the file is there. An absent one is not a refusal, because a
 * default nobody asked for must not be able to stop the daemon; naming a path
 * that is wrong still is, since that is somebody saying they want rules.
 */
function withRulesBeside(
  config: Config,
  path: string,
  exists: (path: string) => boolean,
): Config {
  const candidate = resolve(dirname(path), RULES_FILENAME);
  if (!exists(candidate)) return config;
  return { ...config, agent: { ...config.agent, rulesPath: candidate } };
}
