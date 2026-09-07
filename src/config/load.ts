/**
 * Finding and reading the configuration file.
 *
 * Separate from validating it, so that "the file is not there" and "the file
 * says something impossible" are different failures with different messages.
 */

import { type Config, ConfigError } from "./schema.ts";
import { validateConfig } from "./validate.ts";

/** Environment variable naming the configuration file. */
export const CONFIG_VARIABLE = "ERRAND_CONFIG";

/** Where the configuration is read from when nothing says otherwise. */
export const DEFAULT_CONFIG_PATH = "config.json";

/** Where the configuration will be read from. */
export function configPath(env: Record<string, string | undefined>): string {
  const named = env[CONFIG_VARIABLE];
  return named !== undefined && named.trim().length > 0 ? named.trim() : DEFAULT_CONFIG_PATH;
}

/**
 * Reads and validates the configuration.
 *
 * @throws ConfigError with something actionable: the path that was tried, the
 *   place the file could not be parsed, or every field that was wrong.
 */
export function loadConfig(path: string, read = Deno.readTextFileSync): Config {
  let text: string;
  try {
    text = read(path);
  } catch (error) {
    throw new ConfigError([
      error instanceof Deno.errors.NotFound
        ? `there is no configuration file at ${path}; set ${CONFIG_VARIABLE} or write one there`
        : `the configuration file at ${path} could not be read: ${error}`,
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
