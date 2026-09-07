/**
 * What the host's agent already knows about a provider's models.
 *
 * Two facts are needed to send an image somewhere useful: whether a model
 * accepts one at all, and the endpoint it is reached at. The agent keeps both
 * in a store beside its own configuration, so they are read from there rather
 * than kept as a table here, which would be wrong within a release.
 *
 * Nothing here is configuration. A host with no agent installation yields
 * nothing, and everything that depends on this treats that as "do not route".
 */

import { join } from "@std/path";

/** The model store, inside the agent's configuration directory. */
export const STORE_FILENAME = "models-store.json";

/** One model, reduced to what routing an image needs. */
export interface ModelInfo {
  id: string;
  /** Where the provider is reached. Absent when the store does not say. */
  baseUrl: string | undefined;
  /** Input kinds the model accepts, such as `text` and `image`. */
  input: string[];
  /** Input price, used only to prefer the cheapest model that can see. */
  costIn: number;
}

/**
 * Candidate agent configuration directories, most specific first.
 *
 * The documented default is `~/.pi/agent`, but installs vary and the directory
 * is overridable, so each is tried rather than assumed.
 */
export function agentDirectories(env: Record<string, string | undefined>): string[] {
  const override = env.PI_CODING_AGENT_DIR?.trim();
  const home = env.HOME?.trim() ?? "";
  return [
    ...(override === undefined || override.length === 0 ? [] : [override]),
    join(home, ".pi", "agent"),
    join(env.XDG_CONFIG_HOME?.trim() || join(home, ".config"), "pi"),
  ];
}

/** The first candidate directory that actually holds a model store. */
export function agentDirectory(env: Record<string, string | undefined>): string | undefined {
  for (const directory of agentDirectories(env)) {
    try {
      Deno.statSync(join(directory, STORE_FILENAME));
      return directory;
    } catch {
      // Not this one, or not readable by whoever the daemon runs as.
    }
  }
  return undefined;
}

/** Whether a model can be shown an image. */
export function seesImages(model: ModelInfo | undefined): boolean {
  return model?.input.includes("image") === true;
}

/** Everything the store lists for one provider, or nothing at all. */
export function readModels(directory: string | undefined, provider: string): ModelInfo[] {
  if (directory === undefined) return [];

  let parsed: unknown;
  try {
    parsed = JSON.parse(Deno.readTextFileSync(join(directory, STORE_FILENAME)));
  } catch {
    return [];
  }

  const entry = (parsed as Record<string, unknown> | null)?.[provider];
  const models = (entry as { models?: unknown } | undefined)?.models;
  if (!Array.isArray(models)) return [];

  const found: ModelInfo[] = [];
  for (const raw of models) {
    const model = raw as Record<string, unknown>;
    if (typeof model.id !== "string") continue;
    const cost = model.cost as { input?: unknown } | undefined;
    found.push({
      id: model.id,
      baseUrl: typeof model.baseUrl === "string" ? model.baseUrl : undefined,
      input: Array.isArray(model.input)
        ? model.input.filter((kind): kind is string => typeof kind === "string")
        : [],
      costIn: typeof cost?.input === "number" ? cost.input : Number.POSITIVE_INFINITY,
    });
  }
  return found;
}

/** One model by id, when the store lists it. */
export function modelById(models: ModelInfo[], id: string | undefined): ModelInfo | undefined {
  if (id === undefined) return undefined;
  return models.find((model) => model.id === id);
}

/**
 * The model an image is described by.
 *
 * A named one when the configuration names one, so the choice can be made
 * deliberately. Otherwise the cheapest that can see: describing an image is a
 * paragraph of output, and the model doing the work is a different one.
 *
 * @returns nothing when the provider has no such model, or when the store does
 *   not say where to reach the one it has.
 */
export function visionModel(models: ModelInfo[], preferred?: string): ModelInfo | undefined {
  if (preferred !== undefined) {
    const named = modelById(models, preferred);
    return named !== undefined && seesImages(named) && named.baseUrl !== undefined
      ? named
      : undefined;
  }
  return models
    .filter((model) => seesImages(model) && model.baseUrl !== undefined)
    .sort((left, right) => left.costIn - right.costIn)[0];
}
