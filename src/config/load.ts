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

import { join } from "@std/path";
import { type Config, ConfigError } from "./schema.ts";
import { validateConfig } from "./validate.ts";

/** Environment variable naming the configuration file. */
export const CONFIG_VARIABLE = "ERRAND_CONFIG";

/** The directory name used under a configuration root. */
export const CONFIG_DIRECTORY = "errand";

/** The filename, wherever it is found. */
export const CONFIG_FILENAME = "config.json";

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

  return [
    join(root, CONFIG_DIRECTORY, CONFIG_FILENAME),
    SYSTEM_CONFIG_PATH,
    CONFIG_FILENAME,
  ];
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
    parsed = JSON.parse(text);
  } catch (error) {
    throw new ConfigError([`the configuration file at ${path} is not valid JSON: ${error}`]);
  }

  return validateConfig(parsed);
}
