/**
 * The narrow contract a sandbox backend implements.
 *
 * Sized to exactly what a backend does: start a confined agent and hand back
 * its pipes, say what it can enforce on this host, tear a session down, and
 * find sandboxes a previous run left behind. It is not a plugin system.
 */

import type { AgentProcess } from "../agent/client.ts";
import type { SandboxBackend } from "../config/schema.ts";

/**
 * Where a session's project appears to the agent, under every backend.
 *
 * The agent is never shown a host path. A path carries the operator's name and
 * the shape of their machine, which is not access the sandbox can take back
 * once the agent has read it, and it would then appear in anything the agent
 * writes or says.
 */
export const WORKSPACE_PATH = "/workspace";

/** Where a session's own state directory appears to the agent. */
export const STATE_PATH = "/state";

/** The agent's home, inside the session state it is allowed to write. */
export const AGENT_HOME = `${STATE_PATH}/home`;

/**
 * The one directory ahead of the system ones on a session's PATH.
 *
 * Holds the wrappers a session runs in place of the real program, and nothing
 * else. It is inside the state the agent may write, so a wrapper there is a
 * habit to break rather than a boundary to enforce.
 */
export const AGENT_BIN = `${AGENT_HOME}/bin`;

/** Where the session's agent history lives, as the agent sees it. */
export const AGENT_SESSIONS = `${STATE_PATH}/sessions`;

/** Label marking every sandbox this system owns, for discovery and cleanup. */
export const SYSTEM_LABEL = "errand.system";

/** Label carrying the session a sandbox belongs to. */
export const SESSION_LABEL = "errand.session";

/** Prefix for the name given to a session's sandbox. */
export const SANDBOX_NAME_PREFIX = "errand-";

/** Everything a backend needs to start one session. */
export interface SandboxLaunch {
  /** Stable identifier for this session. */
  sessionId: string;
  /** Absolute host path of the project the agent works in. */
  projectPath: string;
  /** Absolute host path of this session's own state directory. */
  stateDir: string;
  /** Environment given to the agent, holding the provider credential. */
  env: Record<string, string>;
  /** Absolute host path of a file appended to the system prompt. */
  systemPromptPath: string | undefined;
  /** Provider id the agent is started with. */
  provider: string;
  /** Model pattern, or undefined to use the provider's default. */
  model: string | undefined;
  /**
   * Providers the operator defined, written into the agent's configuration.
   *
   * Absent where the operator defined none, which is the ordinary case: the
   * agent then knows only the providers it ships with.
   */
  providers?: Record<string, unknown> | undefined;
  /** Continue the conversation already stored in the state directory. */
  resume: boolean;
}

/** A running sandbox and the handle needed to end it. */
export interface SandboxHandle {
  /** The agent's pipes, for the protocol client. */
  readonly process: AgentProcess;
  /** The sandbox's discoverable name. */
  readonly name: string;
  /**
   * Translates a path as the agent sees it into a path on the host.
   *
   * The agent works at a different root, so a path in a tool call is not a
   * path the daemon can open. Returns undefined for anything outside the
   * project, which is what stops a crafted path from making the daemon read a
   * file the sandbox itself could not.
   */
  toHostPath(agentPath: string): string | undefined;
  /**
   * Stops the sandbox and everything in it, escalating to a kill after the
   * grace period. Safe to call more than once.
   *
   * @returns whether a kill was needed.
   */
  stop(): Promise<boolean>;
}

/** What a backend can and cannot enforce on this host. */
export interface CapabilityReport {
  backend: SandboxBackend;
  /**
   * Guarantees the backend cannot enforce here. Reported at startup, and fatal
   * when the configuration requires full enforcement.
   */
  gaps: string[];
  /** Facts worth stating that are not gaps. */
  notes: string[];
}

/** Raised when a backend cannot run at all, so the daemon refuses to start. */
export class SandboxUnavailableError extends Error {
  constructor(readonly backend: SandboxBackend, readonly reasons: readonly string[]) {
    super(
      `the ${backend} sandbox backend is unavailable:\n${
        reasons.map((reason) => `  - ${reason}`).join("\n")
      }`,
    );
    this.name = "SandboxUnavailableError";
  }
}

/** Raised when starting one session's sandbox fails. */
export class SandboxLaunchError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SandboxLaunchError";
  }
}

/** One sandbox backend. */
export interface Sandbox {
  readonly name: SandboxBackend;

  /**
   * Checks that this backend can run here and reports what it can enforce.
   *
   * @throws SandboxUnavailableError when it cannot run at all. It never falls
   *   back to another backend or to running unconfined.
   */
  probe(): Promise<CapabilityReport>;

  /** Starts one session's sandbox. */
  launch(launch: SandboxLaunch): Promise<SandboxHandle>;

  /** Names of sandboxes this system owns that no live session claims. */
  listOrphans(): Promise<string[]>;

  /** Removes the named sandboxes, returning how many were removed. */
  removeOrphans(names: readonly string[]): Promise<number>;
}

/** The sandbox name for a session, used for discovery and teardown. */
export function sandboxName(sessionId: string): string {
  return `${SANDBOX_NAME_PREFIX}${sessionId}`;
}

/**
 * The command the agent is started with inside any sandbox.
 *
 * The provider is always passed. The agent picks its own default otherwise,
 * which has nothing to do with whichever credential the configuration
 * supplies, so omitting it yields a session that starts and then cannot reach
 * a model.
 */
export function agentCommand(launch: {
  sessionDir: string;
  provider: string;
  model: string | undefined;
  systemPromptPath?: string | undefined;
  resume?: boolean;
}): string[] {
  const command = ["pi", "--mode", "rpc", "--session-dir", launch.sessionDir];
  command.push("--provider", launch.provider);
  // Memory is appended to the system prompt from a file, which costs the agent
  // no tool call to read and no round trip to a daemon it cannot reach.
  if (launch.systemPromptPath !== undefined) {
    command.push("--append-system-prompt", launch.systemPromptPath);
  }
  if (launch.model !== undefined && launch.model.length > 0) {
    command.push("--model", launch.model);
  }
  // The agent keeps its history in the session directory, so continuing there
  // is what makes a resumed thread pick up the conversation rather than start
  // a new one that happens to share a directory.
  if (launch.resume === true) command.push("--continue");
  return command;
}
