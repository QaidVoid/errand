/**
 * Removes secret values from anything about to be logged or posted.
 *
 * Two layers, because either alone leaks. Structural redaction blanks the
 * known secret fields when the effective configuration is rendered. Value
 * redaction scrubs the same strings out of arbitrary text, since an error
 * thrown by a library may carry a token that never passed through the config
 * renderer.
 */

import type { Config } from "./schema.ts";
import { SECRET_PATHS } from "./schema.ts";

/** What a secret is replaced with. Fixed so it is greppable in a log. */
export const REDACTION = "[redacted]";

/** Shortest secret worth scrubbing from free text, to avoid mangling prose. */
const MIN_SCRUBBABLE_LENGTH = 8;

function readPath(source: unknown, path: string): unknown {
  let current: unknown = source;
  for (const segment of path.split(".")) {
    if (typeof current !== "object" || current === null) return undefined;
    current = (current as Record<string, unknown>)[segment];
  }
  return current;
}

/**
 * Renders the effective configuration with every secret field blanked, for
 * logging at startup.
 */
export function redactConfig(config: Config): Record<string, unknown> {
  const clone = structuredClone(config) as unknown as Record<string, unknown>;
  for (const path of SECRET_PATHS) {
    const segments = path.split(".");
    const last = segments.pop();
    if (last === undefined) continue;
    const parent = readPath(clone, segments.join("."));
    if (typeof parent === "object" && parent !== null) {
      (parent as Record<string, unknown>)[last] = REDACTION;
    }
  }
  return clone;
}

/** Collects the secret values held by a configuration, for scrubbing text. */
export function secretValues(config: Config): string[] {
  const values: string[] = [];
  for (const path of SECRET_PATHS) {
    const value = readPath(config, path);
    if (typeof value === "string" && value.length >= MIN_SCRUBBABLE_LENGTH) {
      values.push(value);
    }
  }
  // A defined provider carries its own credential, and there is no fixed path
  // to name it by: the operator chooses how many providers there are and what
  // they are called. Walked instead, so a second provider's key is scrubbed as
  // thoroughly as the first one's.
  for (const definition of Object.values(config.agent.providers)) {
    if (typeof definition !== "object" || definition === null) continue;
    const credential = (definition as Record<string, unknown>).credential;
    if (typeof credential === "string" && credential.length >= MIN_SCRUBBABLE_LENGTH) {
      values.push(credential);
    }
  }
  return values;
}

/**
 * Scrubs known secret values out of arbitrary text.
 *
 * A secret shorter than the minimum is left alone: replacing a short string
 * everywhere it occurs mangles ordinary prose without hiding anything worth
 * hiding.
 */
export function redactText(text: string, secrets: readonly string[]): string {
  let scrubbed = text;
  for (const secret of secrets) {
    if (secret.length < MIN_SCRUBBABLE_LENGTH) continue;
    scrubbed = scrubbed.split(secret).join(REDACTION);
  }
  return scrubbed;
}
