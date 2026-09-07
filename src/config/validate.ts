/**
 * Turns whatever was on disk into a {@link Config}, or refuses with reasons.
 *
 * Every problem is collected before anything is thrown, and an unknown key is
 * a problem rather than something quietly ignored: a misspelled setting that
 * takes no effect is worse than one that is rejected, because the daemon then
 * runs with a guarantee somebody believes they configured.
 */

import { isAbsolute, resolve } from "@std/path";
import { parseSize } from "./size.ts";
import {
  type AgentConfig,
  type ChatConfig,
  type Config,
  ConfigError,
  DEFAULTS,
  type GithubConfig,
  type LimitsConfig,
  type NetworkMode,
  type OutputConfig,
  type SandboxBackend,
  type SandboxConfig,
  type ShutdownConfig,
  type TimeoutsConfig,
  type WebConfig,
} from "./schema.ts";

const BACKENDS: SandboxBackend[] = ["podman", "bailey"];
const NETWORKS: NetworkMode[] = ["restricted", "none"];

const KNOWN = {
  root: [
    "chat",
    "agent",
    "github",
    "projectRoot",
    "stateDir",
    "sandbox",
    "output",
    "shutdown",
    "web",
    "limits",
    "timeouts",
  ],
  chat: ["token", "channelId", "allowedUserIds", "blockedUserIds", "operatorUserIds"],
  agent: ["provider", "model", "visionModel", "credentialName", "credential"],
  github: ["token", "userName", "userEmail"],
  sandbox: Object.keys(DEFAULTS.sandbox),
  shutdown: ["allowedUserIds"],
  web: ["host", "port", "observer", "publicUrl"],
  output: Object.keys(DEFAULTS.output),
  limits: Object.keys(DEFAULTS.limits),
  timeouts: Object.keys(DEFAULTS.timeouts),
} as const;

/** Collects reasons so that a first run reports all of them at once. */
class Problems {
  readonly found: string[] = [];

  add(problem: string): void {
    this.found.push(problem);
  }
}

function section(source: Record<string, unknown>, name: string): Record<string, unknown> {
  const value = source[name];
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function rejectUnknown(
  source: Record<string, unknown>,
  known: readonly string[],
  where: string,
  problems: Problems,
): void {
  for (const key of Object.keys(source)) {
    if (!known.includes(key)) {
      problems.add(`${where}.${key} is not a setting; check the spelling`);
    }
  }
}

function requiredString(
  source: Record<string, unknown>,
  key: string,
  where: string,
  problems: Problems,
): string {
  const value = source[key];
  if (typeof value !== "string" || value.trim().length === 0) {
    problems.add(`${where}.${key} is required and must be a non-empty string`);
    return "";
  }
  return value.trim();
}

function optionalString(
  source: Record<string, unknown>,
  key: string,
  where: string,
  problems: Problems,
): string | undefined {
  const value = source[key];
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.trim().length === 0) {
    problems.add(`${where}.${key} must be a non-empty string when it is set`);
    return undefined;
  }
  return value.trim();
}

function idList(
  source: Record<string, unknown>,
  key: string,
  where: string,
  problems: Problems,
): string[] {
  const value = source[key];
  if (value === undefined) return [];
  if (!Array.isArray(value)) {
    problems.add(`${where}.${key} must be a list of account ids`);
    return [];
  }
  const ids: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim().length === 0) {
      problems.add(`${where}.${key} contains an entry that is not an account id`);
      continue;
    }
    ids.push(entry.trim());
  }
  return ids;
}

function positive(
  source: Record<string, unknown>,
  key: string,
  fallback: number,
  where: string,
  problems: Problems,
): number {
  const value = source[key];
  if (value === undefined) return fallback;
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    problems.add(`${where}.${key} must be a number greater than zero`);
    return fallback;
  }
  return value;
}

function size(
  source: Record<string, unknown>,
  key: string,
  fallback: string,
  where: string,
  problems: Problems,
): string {
  const value = source[key];
  if (value === undefined) return fallback;
  if (typeof value !== "string" || parseSize(value) === undefined) {
    problems.add(`${where}.${key} must be a size such as 512m or 4g`);
    return fallback;
  }
  return value.trim();
}

function flag(
  source: Record<string, unknown>,
  key: string,
  fallback: boolean,
  where: string,
  problems: Problems,
): boolean {
  const value = source[key];
  if (value === undefined) return fallback;
  if (typeof value !== "boolean") {
    problems.add(`${where}.${key} must be true or false`);
    return fallback;
  }
  return value;
}

function directory(
  source: Record<string, unknown>,
  key: string,
  problems: Problems,
): string {
  const value = source[key];
  if (typeof value !== "string" || value.trim().length === 0) {
    problems.add(`${key} is required and must be an absolute path`);
    return "";
  }
  const path = value.trim();
  if (!isAbsolute(path)) {
    problems.add(`${key} must be an absolute path, got ${path}`);
    return "";
  }
  return resolve(path);
}

function validateChat(raw: Record<string, unknown>, problems: Problems): ChatConfig {
  const source = section(raw, "chat");
  rejectUnknown(source, KNOWN.chat, "chat", problems);

  const allowed = idList(source, "allowedUserIds", "chat", problems);
  if (allowed.length === 0) {
    problems.add(
      "chat.allowedUserIds is required and must list at least one account; there is no allow-everyone default",
    );
  }

  return {
    token: requiredString(source, "token", "chat", problems),
    channelId: requiredString(source, "channelId", "chat", problems),
    allowedUserIds: allowed,
    blockedUserIds: idList(source, "blockedUserIds", "chat", problems),
    operatorUserIds: idList(source, "operatorUserIds", "chat", problems),
  };
}

function validateAgent(raw: Record<string, unknown>, problems: Problems): AgentConfig {
  const source = section(raw, "agent");
  rejectUnknown(source, KNOWN.agent, "agent", problems);

  return {
    provider: requiredString(source, "provider", "agent", problems),
    model: optionalString(source, "model", "agent", problems),
    visionModel: optionalString(source, "visionModel", "agent", problems),
    credentialName: requiredString(source, "credentialName", "agent", problems),
    credential: requiredString(source, "credential", "agent", problems),
  };
}

/**
 * Reads the GitHub identity, which the whole section may omit.
 *
 * Present but incomplete is a problem rather than a partial identity: a
 * session that pushes as half of somebody is worse than one that cannot push.
 */
function validateGithub(
  raw: Record<string, unknown>,
  problems: Problems,
): GithubConfig | undefined {
  if (raw.github === undefined) return undefined;
  const source = section(raw, "github");
  rejectUnknown(source, KNOWN.github, "github", problems);

  return {
    token: requiredString(source, "token", "github", problems),
    userName: requiredString(source, "userName", "github", problems),
    userEmail: requiredString(source, "userEmail", "github", problems),
  };
}

function validateSandbox(raw: Record<string, unknown>, problems: Problems): SandboxConfig {
  const source = section(raw, "sandbox");
  rejectUnknown(source, KNOWN.sandbox, "sandbox", problems);
  const defaults = DEFAULTS.sandbox;

  const backend = source.backend ?? defaults.backend;
  if (!BACKENDS.includes(backend as SandboxBackend)) {
    problems.add(`sandbox.backend must be one of ${BACKENDS.join(", ")}`);
  }
  const network = source.network ?? defaults.network;
  if (!NETWORKS.includes(network as NetworkMode)) {
    problems.add(`sandbox.network must be one of ${NETWORKS.join(", ")}`);
  }

  return {
    backend:
      (BACKENDS.includes(backend as SandboxBackend) ? backend : defaults.backend) as SandboxBackend,
    network:
      (NETWORKS.includes(network as NetworkMode) ? network : defaults.network) as NetworkMode,
    image: optionalString(source, "image", "sandbox", problems) ?? defaults.image,
    requireFullEnforcement: flag(
      source,
      "requireFullEnforcement",
      defaults.requireFullEnforcement,
      "sandbox",
      problems,
    ),
    memory: size(source, "memory", defaults.memory, "sandbox", problems),
    cpus: positive(source, "cpus", defaults.cpus, "sandbox", problems),
    pids: positive(source, "pids", defaults.pids, "sandbox", problems),
    fileMax: size(source, "fileMax", defaults.fileMax, "sandbox", problems),
    disk: size(source, "disk", defaults.disk, "sandbox", problems),
    diskCheckMs: positive(source, "diskCheckMs", defaults.diskCheckMs, "sandbox", problems),
    gracePeriodMs: positive(source, "gracePeriodMs", defaults.gracePeriodMs, "sandbox", problems),
  };
}

function validateOutput(raw: Record<string, unknown>, problems: Problems): OutputConfig {
  const source = section(raw, "output");
  rejectUnknown(source, KNOWN.output, "output", problems);
  const defaults = DEFAULTS.output;

  return {
    forwardToolOutput: flag(
      source,
      "forwardToolOutput",
      defaults.forwardToolOutput,
      "output",
      problems,
    ),
    maxToolOutputChars: positive(
      source,
      "maxToolOutputChars",
      defaults.maxToolOutputChars,
      "output",
      problems,
    ),
    maxAttachmentBytes: positive(
      source,
      "maxAttachmentBytes",
      defaults.maxAttachmentBytes,
      "output",
      problems,
    ),
    maxAttachmentsPerMessage: positive(
      source,
      "maxAttachmentsPerMessage",
      defaults.maxAttachmentsPerMessage,
      "output",
      problems,
    ),
    postDiffs: flag(source, "postDiffs", defaults.postDiffs, "output", problems),
  };
}

/**
 * Reads who may power off the host.
 *
 * An absent section means nobody, which is the safe reading of silence for a
 * command that acts on the machine.
 */
function validateShutdown(raw: Record<string, unknown>, problems: Problems): ShutdownConfig {
  const source = section(raw, "shutdown");
  rejectUnknown(source, KNOWN.shutdown, "shutdown", problems);
  return { allowedUserIds: idList(source, "allowedUserIds", "shutdown", problems) };
}

/**
 * Reads the interface's settings, when one is configured at all.
 *
 * The address is not checked here. Whether it is one worth serving over is the
 * interface's own rule, and it is applied where the listener is opened so that
 * a refusal names the listener.
 */
function validateWeb(raw: Record<string, unknown>, problems: Problems): WebConfig | undefined {
  if (raw.web === undefined) return undefined;
  const source = section(raw, "web");
  rejectUnknown(source, KNOWN.web, "web", problems);
  const defaults = DEFAULTS.web;

  return {
    host: optionalString(source, "host", "web", problems) ?? defaults.host,
    port: positive(source, "port", defaults.port, "web", problems),
    observer: flag(source, "observer", defaults.observer, "web", problems),
    publicUrl: optionalString(source, "publicUrl", "web", problems),
  };
}

function validateLimits(raw: Record<string, unknown>, problems: Problems): LimitsConfig {
  const source = section(raw, "limits");
  rejectUnknown(source, KNOWN.limits, "limits", problems);
  const defaults = DEFAULTS.limits;

  return {
    maxConcurrentTurns: positive(
      source,
      "maxConcurrentTurns",
      defaults.maxConcurrentTurns,
      "limits",
      problems,
    ),
    maxLiveSessions: positive(
      source,
      "maxLiveSessions",
      defaults.maxLiveSessions,
      "limits",
      problems,
    ),
    maxQueueLength: positive(source, "maxQueueLength", defaults.maxQueueLength, "limits", problems),
    maxQueueWaitMs: positive(source, "maxQueueWaitMs", defaults.maxQueueWaitMs, "limits", problems),
  };
}

function validateTimeouts(raw: Record<string, unknown>, problems: Problems): TimeoutsConfig {
  const source = section(raw, "timeouts");
  rejectUnknown(source, KNOWN.timeouts, "timeouts", problems);
  const defaults = DEFAULTS.timeouts;

  return {
    idleMs: positive(source, "idleMs", defaults.idleMs, "timeouts", problems),
    startupMs: positive(source, "startupMs", defaults.startupMs, "timeouts", problems),
    questionMs: positive(source, "questionMs", defaults.questionMs, "timeouts", problems),
    abortMs: positive(source, "abortMs", defaults.abortMs, "timeouts", problems),
  };
}

/**
 * Validates a parsed configuration file.
 *
 * Throws {@link ConfigError} carrying every problem found, so that a first run
 * is fixed in one pass rather than one message at a time.
 */
export function validateConfig(parsed: unknown): Config {
  const problems = new Problems();
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new ConfigError(["the configuration file must contain a JSON object"]);
  }
  const raw = parsed as Record<string, unknown>;
  rejectUnknown(raw, KNOWN.root, "config", problems);

  const config: Config = {
    chat: validateChat(raw, problems),
    agent: validateAgent(raw, problems),
    github: validateGithub(raw, problems),
    projectRoot: directory(raw, "projectRoot", problems),
    stateDir: directory(raw, "stateDir", problems),
    sandbox: validateSandbox(raw, problems),
    output: validateOutput(raw, problems),
    shutdown: validateShutdown(raw, problems),
    web: validateWeb(raw, problems),
    limits: validateLimits(raw, problems),
    timeouts: validateTimeouts(raw, problems),
  };

  if (config.projectRoot.length > 0 && config.projectRoot === config.stateDir) {
    problems.add("projectRoot and stateDir must be different directories");
  }

  if (problems.found.length > 0) throw new ConfigError(problems.found);
  return config;
}
