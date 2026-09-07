/**
 * The shape of the daemon's configuration, and its documented defaults.
 *
 * Every optional field on disk is a required field here, filled from
 * {@link DEFAULTS}, so nothing downstream reasons about absence.
 *
 * This grows with the daemon. A field is added when something reads it, not in
 * anticipation of something that might.
 */

/** Which sandbox confines a session's agent. There is no unsandboxed option. */
export type SandboxBackend = "podman" | "bailey";

/** How much network a session gets. `none` disables it entirely. */
export type NetworkMode = "restricted" | "none";

/** Chat connection and who may drive the bot. */
export interface ChatConfig {
  /** Bot token. Secret. Never enters a sandbox. */
  token: string;
  /** The single channel the daemon serves. Everything else is ignored. */
  channelId: string;
  /**
   * Accounts permitted to drive sessions. Must not be empty.
   *
   * The single entry {@link ALLOW_EVERY_USER} opens it to everyone who can
   * post in the served channel. That is a deliberate, visible choice: an empty
   * list is still a refusal to start, so nobody arrives at open access by
   * leaving a field blank.
   */
  allowedUserIds: string[];
  /**
   * Accounts refused before anything else is considered.
   *
   * Ahead of the allowlist and of any session role, so excluding somebody is
   * one decision rather than an audit of every list they appear on.
   */
  blockedUserIds: string[];
  /** Accounts that may control any session, not only their own. */
  operatorUserIds: string[];
}

/** Which model the agent talks to, and the credential it reaches it with. */
export interface AgentConfig {
  /** Provider id, such as `anthropic` or `zai-coding-cn`. */
  provider: string;
  /** Model pattern or id. Omit to use the provider's default. */
  model: string | undefined;
  /** Environment variable the agent reads, such as `ANTHROPIC_API_KEY`. */
  credentialName: string;
  /** The credential value. Secret. */
  credential: string;
}

/** What a session may consume, and what the backend enforces. */
export interface SandboxConfig {
  /** Which backend confines sessions. */
  backend: SandboxBackend;
  /**
   * Refuse to start when the backend cannot enforce every configured
   * guarantee on this host, rather than reporting the gap and continuing.
   */
  requireFullEnforcement: boolean;
  /** Network exposure granted to a session. */
  network: NetworkMode;
  /** Container image the podman backend runs. Inert under bailey. */
  image: string;
  /** Memory ceiling per session, in size syntax such as `4g`. */
  memory: string;
  /** CPU ceiling per session, in cores. */
  cpus: number;
  /** Process count ceiling per session. */
  pids: number;
  /** Largest single file a session may write, in size syntax. */
  fileMax: string;
  /**
   * How much a session may add to its project and state together.
   *
   * Measured rather than enforced, because no backend caps what a process tree
   * writes in aggregate without root. Passing it ends the session.
   */
  disk: string;
  /** How often a session's disk use is measured, in milliseconds. */
  diskCheckMs: number;
  /** How long a sandbox may take to stop before it is killed. */
  gracePeriodMs: number;
}

/** Bounds on how much work exists at once. */
export interface LimitsConfig {
  /** Sessions that may have a model turn in flight simultaneously. */
  maxConcurrentTurns: number;
  /** Sessions that may exist at all. */
  maxLiveSessions: number;
  /** Prompts that may wait for a turn slot. */
  maxQueueLength: number;
  /** How long a queued prompt may wait before it expires unsent. */
  maxQueueWaitMs: number;
}

/** Deadlines that end or unblock a session. */
export interface TimeoutsConfig {
  /** No message and no agent activity for this long ends the session. */
  idleMs: number;
  /** How long the agent has to become ready before it is abandoned. */
  startupMs: number;
  /** How long a question posted to a thread waits for an answer. */
  questionMs: number;
  /** How long an abort waits for the agent before it is forced. */
  abortMs: number;
}

/** The fully resolved configuration the daemon runs on. */
export interface Config {
  chat: ChatConfig;
  agent: AgentConfig;
  /**
   * Absolute path under which every session's project directory lives.
   *
   * A session works in a subdirectory of this and nowhere else.
   */
  projectRoot: string;
  /** Where per-session state directories are created on the host. */
  stateDir: string;
  sandbox: SandboxConfig;
  limits: LimitsConfig;
  timeouts: TimeoutsConfig;
}

/**
 * Allowlist entry that admits everyone who can post in the served channel.
 *
 * Anyone admitted can run code in a sandbox with write access to the project
 * root, so this is only reasonable when the channel itself is the boundary.
 */
export const ALLOW_EVERY_USER = "*";

/** Field paths whose values must never be logged, posted, or reported. */
export const SECRET_PATHS = ["chat.token", "agent.credential"] as const;

/** Values used for any optional field the configuration file omits. */
export const DEFAULTS = {
  sandbox: {
    backend: "bailey" as SandboxBackend,
    requireFullEnforcement: true,
    network: "restricted" as NetworkMode,
    image: "localhost/errand-agent:latest",
    memory: "4g",
    cpus: 2,
    pids: 512,
    fileMax: "1g",
    disk: "5g",
    diskCheckMs: 30_000,
    gracePeriodMs: 10_000,
  },
  limits: {
    maxConcurrentTurns: 2,
    maxLiveSessions: 4,
    maxQueueLength: 32,
    maxQueueWaitMs: 900_000,
  },
  timeouts: {
    idleMs: 1_800_000,
    startupMs: 60_000,
    questionMs: 300_000,
    abortMs: 15_000,
  },
} as const;

/**
 * Every reason the configuration was rejected, not just the first.
 *
 * Reporting one at a time makes a first run a guessing game, so the daemon
 * refuses with the whole list.
 */
export class ConfigError extends Error {
  readonly problems: readonly string[];

  constructor(problems: readonly string[]) {
    super(`configuration rejected:\n${problems.map((problem) => `  - ${problem}`).join("\n")}`);
    this.name = "ConfigError";
    this.problems = problems;
  }
}
