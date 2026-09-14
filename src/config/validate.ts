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
  type DelegateConfig,
  type GithubConfig,
  type LimitsConfig,
  type NetworkMode,
  type OutputConfig,
  type PolicyExtraConfig,
  type SandboxBackend,
  type SandboxConfig,
  type ShutdownConfig,
  type TimeoutsConfig,
  type WebConfig,
} from "./schema.ts";

const BACKENDS: SandboxBackend[] = ["podman", "bailey"];
const NETWORKS: NetworkMode[] = ["restricted", "none"];

/** What a variable may be called, which is what a shell would accept. */
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Variables the policy sets itself, and so refuses to take from a file. */
const SET_BY_THE_POLICY = ["PATH", "HOME"];

const KNOWN = {
  root: [
    // Not a setting: it is how an editor finds the schema to check the file
    // against, and refusing it would make the file uncheckable.
    "$schema",
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
  chat: [
    "token",
    "channelId",
    "allowedUserIds",
    "blockedUserIds",
    "operatorUserIds",
    "startOnMention",
  ],
  agent: [
    "provider",
    "model",
    "visionModel",
    "credentialName",
    "credential",
    "delegate",
    "rulesPath",
  ],
  delegate: ["model", "perTurn", "deadlineMs", "baseUrl"],
  github: ["token", "userName", "userEmail"],
  sandbox: [...Object.keys(DEFAULTS.sandbox), "policyExtra", "pathExtra", "env"],
  policyExtra: ["read", "write", "execute"],
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

/**
 * Reads a list of outbound ports.
 *
 * A port is a whole number in the range a socket accepts, so anything outside
 * it is refused rather than clamped. An empty list would leave a session that
 * has a network unable to open anything, which is a mistake worth naming.
 */
function ports(
  source: Record<string, unknown>,
  key: string,
  fallback: number[],
  where: string,
  problems: Problems,
): number[] {
  const value = source[key];
  if (value === undefined) return fallback;
  if (!Array.isArray(value)) {
    problems.add(`${where}.${key} must be a list of port numbers`);
    return fallback;
  }
  const found: number[] = [];
  for (const entry of value) {
    if (typeof entry !== "number" || !Number.isInteger(entry) || entry < 1 || entry > 65535) {
      problems.add(`${where}.${key} contains ${entry}, which is not a port between 1 and 65535`);
      continue;
    }
    found.push(entry);
  }
  if (found.length === 0) {
    problems.add(`${where}.${key} names no port; remove it for the default, or name one`);
    return fallback;
  }
  return found;
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
    startOnMention: flag(
      source,
      "startOnMention",
      DEFAULTS.chat.startOnMention,
      "chat",
      problems,
    ),
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
    delegate: validateDelegate(source, problems),
    rulesPath: optionalAbsolutePath(source, "rulesPath", "agent", problems),
  };
}

/**
 * An optional path that must be absolute when it is given at all.
 *
 * Absolute because the daemon's working directory is not the operator's, so a
 * relative path names a different file depending on where the daemon was
 * started. Whether the file is actually there is checked at startup, where a
 * refusal can say so; this stays free of the filesystem so it remains a pure
 * reading of the configuration.
 */
function optionalAbsolutePath(
  source: Record<string, unknown>,
  key: string,
  where: string,
  problems: Problems,
): string | undefined {
  const value = source[key];
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.trim().length === 0) {
    problems.add(`${where}.${key} must be an absolute path`);
    return undefined;
  }
  const path = value.trim();
  if (!isAbsolute(path)) {
    problems.add(`${where}.${key} must be an absolute path, got ${path}`);
    return undefined;
  }
  return resolve(path);
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

/**
 * Reads the delegation settings, which the whole section may omit.
 *
 * Naming no model is the same as having no section: there is nothing to ask,
 * so nothing is offered to the agent.
 */
function validateDelegate(
  source: Record<string, unknown>,
  problems: Problems,
): DelegateConfig | undefined {
  if (source.delegate === undefined) return undefined;
  const section_ = section(source, "delegate");
  rejectUnknown(section_, KNOWN.delegate, "agent.delegate", problems);
  const defaults = DEFAULTS.delegate;

  return {
    model: requiredString(section_, "model", "agent.delegate", problems),
    perTurn: positive(section_, "perTurn", defaults.perTurn, "agent.delegate", problems),
    deadlineMs: positive(section_, "deadlineMs", defaults.deadlineMs, "agent.delegate", problems),
    baseUrl: optionalString(section_, "baseUrl", "agent.delegate", problems),
  };
}

/**
 * Reads a list of absolute paths.
 *
 * A relative path in a policy is meaningless: there is no working directory to
 * resolve it against once the session has pivoted, so it is refused rather
 * than resolved against whatever the daemon happened to be started from.
 */
function pathList(
  source: Record<string, unknown>,
  key: string,
  where: string,
  problems: Problems,
): string[] {
  const value = source[key];
  if (value === undefined) return [];
  if (!Array.isArray(value)) {
    problems.add(`${where}.${key} must be a list of absolute paths`);
    return [];
  }

  const paths: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim().length === 0) {
      problems.add(`${where}.${key} contains an entry that is not a path`);
      continue;
    }
    const path = entry.trim();
    if (!isAbsolute(path)) {
      problems.add(`${where}.${key} entry ${path} must be an absolute path`);
      continue;
    }
    paths.push(resolve(path));
  }
  return paths;
}

/** Reads the paths granted on top of the generated policy, if any. */
function validatePolicyExtra(
  source: Record<string, unknown>,
  problems: Problems,
): PolicyExtraConfig | undefined {
  if (source.policyExtra === undefined) return undefined;
  const extra = section(source, "policyExtra");
  rejectUnknown(extra, KNOWN.policyExtra, "sandbox.policyExtra", problems);

  const read = pathList(extra, "read", "sandbox.policyExtra", problems);
  const write = pathList(extra, "write", "sandbox.policyExtra", problems);
  const execute = pathList(extra, "execute", "sandbox.policyExtra", problems);

  if (read.length === 0 && write.length === 0 && execute.length === 0) {
    problems.add("sandbox.policyExtra is set but grants nothing; remove it, or name a path");
  }
  return { read, write, execute };
}

/** Reads the directories added to a session's PATH, if any. */
function validatePathExtra(
  source: Record<string, unknown>,
  problems: Problems,
): string[] | undefined {
  if (source.pathExtra === undefined) return undefined;
  const paths = pathList(source, "pathExtra", "sandbox", problems);
  if (paths.length === 0 && problems.found.length === 0) {
    problems.add("sandbox.pathExtra is set but names nothing; remove it, or name a directory");
  }
  return paths;
}

/**
 * Reads the variables set in every session's environment, if any.
 *
 * `PATH` and `HOME` are refused rather than merged: the policy sets both to
 * paths it places, and a session pointed at anything else would be naming
 * paths no grant covers.
 */
function validateSandboxEnv(
  source: Record<string, unknown>,
  problems: Problems,
): Record<string, string> | undefined {
  if (source.env === undefined) return undefined;
  const raw = section(source, "env");

  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(raw)) {
    if (!ENV_NAME.test(name)) {
      problems.add(`sandbox.env.${name} is not a variable name`);
      continue;
    }
    if (SET_BY_THE_POLICY.includes(name)) {
      problems.add(`sandbox.env must not set ${name}, which the policy sets itself`);
      continue;
    }
    if (typeof value !== "string") {
      problems.add(`sandbox.env.${name} must be a string`);
      continue;
    }
    env[name] = value;
  }

  if (Object.keys(env).length === 0 && problems.found.length === 0) {
    problems.add("sandbox.env is set but names nothing; remove it, or name a variable");
  }
  return env;
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
    egressPorts: ports(source, "egressPorts", [...defaults.egressPorts], "sandbox", problems),
    hideHostAddress: flag(
      source,
      "hideHostAddress",
      defaults.hideHostAddress,
      "sandbox",
      problems,
    ),
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
    policyExtra: validatePolicyExtra(source, problems),
    pathExtra: validatePathExtra(source, problems),
    env: validateSandboxEnv(source, problems),
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

  // Asked once both sections are read, since the name is the operator's own.
  // Shadowing it would authenticate the agent with whatever was set here.
  if (config.sandbox.env?.[config.agent.credentialName] !== undefined) {
    problems.add(
      `sandbox.env must not set ${config.agent.credentialName}, which carries the provider credential`,
    );
  }

  if (config.projectRoot.length > 0 && config.projectRoot === config.stateDir) {
    problems.add("projectRoot and stateDir must be different directories");
  }

  if (problems.found.length > 0) throw new ConfigError(problems.found);
  return config;
}
