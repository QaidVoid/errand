/**
 * Container backend using rootless podman.
 *
 * The hardening flags below are not a starting point to tune. Each was
 * measured against a real container, and the network options in particular are
 * the difference between a container that can reach host services and one that
 * cannot.
 */

import { parseSize } from "../config/size.ts";
import type { SandboxConfig } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import {
  AGENT_HOME,
  AGENT_SESSIONS,
  agentCommand,
  type CapabilityReport,
  type Sandbox,
  type SandboxHandle,
  type SandboxLaunch,
  SandboxLaunchError,
  sandboxName,
  SandboxUnavailableError,
  SESSION_LABEL,
  STATE_PATH,
  SYSTEM_LABEL,
  WORKSPACE_PATH,
} from "./backend.ts";
import { hostPathUnder } from "./paths.ts";
import { spawnAgent } from "./spawn.ts";

/*
 * The agent's home is not a tmpfs. A tmpfs is owned by the user namespace's
 * root, and with `--userns=keep-id` the agent is not that user, so it could not
 * write there. The agent stores credential state under its home, so an
 * unwritable home rejects every prompt with a permission error.
 */

/**
 * Network arguments that leave the model provider reachable and the host not.
 *
 * Measured on podman 5.8.2 with pasta 2025.12.15: podman's default maps the
 * host at `169.254.1.2`, from which a host service bound to `0.0.0.0` is
 * reachable. Disabling both mappings closes that path while outbound internet
 * and DNS keep working. The legacy `--no-map-gw` spelling does not close it.
 */
export const RESTRICTED_NETWORK = "pasta:--map-host-loopback,none,--map-guest-addr,none";

/** Flags that would undo the isolation this backend exists to provide. */
export const FORBIDDEN_ARGS = [
  "--privileged",
  "--pid=host",
  "--ipc=host",
  "--network=host",
  "--userns=host",
  "--cap-add",
  "docker.sock",
  "podman.sock",
];

/** Runs podman and collects what it said. Injected for tests. */
export type Run = (
  args: readonly string[],
) => Promise<{ code: number; stdout: string; stderr: string }>;

const runPodman: Run = async (args) => {
  const { code, stdout, stderr } = await new Deno.Command("podman", {
    args: [...args],
    stdout: "piped",
    stderr: "piped",
  }).output();
  const decoder = new TextDecoder();
  return { code, stdout: decoder.decode(stdout), stderr: decoder.decode(stderr) };
};

/**
 * Builds the full podman argument list for a session.
 *
 * Pure, so a test can assert the exact flags without starting a container.
 */
export function podmanArgs(config: SandboxConfig, launch: SandboxLaunch): string[] {
  const name = sandboxName(launch.sessionId);
  const network = config.network === "none" ? "none" : RESTRICTED_NETWORK;

  const args = [
    "run",
    "--interactive",
    "--rm",
    "--name",
    name,
    "--label",
    `${SYSTEM_LABEL}=true`,
    "--label",
    `${SESSION_LABEL}=${launch.sessionId}`,
    "--userns=keep-id",
    "--read-only",
    "--tmpfs",
    "/tmp",
    "--volume",
    `${launch.projectPath}:${WORKSPACE_PATH}:rw,Z`,
    "--volume",
    `${launch.stateDir}:${STATE_PATH}:rw,Z`,
    "--workdir",
    WORKSPACE_PATH,
    "--cap-drop=ALL",
    "--security-opt",
    "no-new-privileges",
    `--network=${network}`,
    "--memory",
    config.memory,
    "--cpus",
    String(config.cpus),
    "--pids-limit",
    String(config.pids),
    // An fsize ulimit, in bytes, inherited by every process in the container.
    // It caps one file rather than total usage, which no container runtime can
    // bound without a sized filesystem underneath it.
    "--ulimit",
    `fsize=${parseSize(config.fileMax) ?? 0}`,
    "--env",
    `HOME=${AGENT_HOME}`,
  ];

  // Ahead of the daemon's own, so a name it sets keeps the daemon's value.
  for (const [key, value] of Object.entries(config.env ?? {})) {
    args.push("--env", `${key}=${value}`);
  }

  for (const [key, value] of Object.entries(launch.env)) {
    args.push("--env", `${key}=${value}`);
  }

  args.push(
    config.image,
    ...agentCommand({
      sessionDir: AGENT_SESSIONS,
      provider: launch.provider,
      model: launch.model,
      systemPromptPath: launch.systemPromptPath === undefined
        ? undefined
        : `${STATE_PATH}/${launch.systemPromptPath.split("/").pop()}`,
      resume: launch.resume,
    }),
  );
  return args;
}

/** Rootless podman, one container per session. */
export class PodmanSandbox implements Sandbox {
  readonly name = "podman" as const;

  constructor(
    private readonly config: SandboxConfig,
    private readonly log: Logger,
    private readonly run: Run = runPodman,
  ) {}

  async probe(): Promise<CapabilityReport> {
    const reasons: string[] = [];

    const info = await this.run(["info", "--format", "{{.Host.Security.Rootless}}"]).catch(
      () => null,
    );
    if (info === null || info.code !== 0) {
      reasons.push("podman is not installed, or `podman info` failed");
    } else if (info.stdout.trim() !== "true") {
      reasons.push("podman is not running rootless, which this backend requires");
    }

    if (reasons.length === 0) {
      const image = await this.run(["image", "exists", this.config.image]);
      if (image.code !== 0) {
        reasons.push(
          `the configured image ${this.config.image} is not present; build it before starting`,
        );
      }
    }

    if (reasons.length > 0) throw new SandboxUnavailableError("podman", reasons);

    const notes = [
      `sessions run in rootless containers from ${this.config.image}`,
      this.config.network === "none"
        ? "sessions have no network, so the agent cannot reach a model provider"
        : "sessions reach the model provider, and host services are unreachable",
      `limits per session: memory ${this.config.memory}, cpus ${this.config.cpus}, pids ${this.config.pids}`,
      `no single file may exceed ${this.config.fileMax}, set as an fsize ulimit on the container`,
      // A note rather than a gap: the daemon never claims to enforce a disk
      // total, so calling it an unenforceable guarantee would make
      // requireFullEnforcement refuse to start on every host forever.
      `a session is stopped once it has written ${this.config.disk}, which is measured rather than enforced`,
    ];

    return { backend: "podman", gaps: [], notes };
  }

  launch(launch: SandboxLaunch): Promise<SandboxHandle> {
    const name = sandboxName(launch.sessionId);
    const spawned = spawnAgent("podman", podmanArgs(this.config, launch));
    this.log.info("container started", { session: launch.sessionId, name });

    let stopped = false;
    return Promise.resolve({
      process: spawned.process,
      name,
      toHostPath: (agentPath: string) =>
        hostPathUnder(WORKSPACE_PATH, launch.projectPath, agentPath),
      stop: async (): Promise<boolean> => {
        if (stopped) return false;
        stopped = true;

        const graceSeconds = Math.max(1, Math.round(this.config.gracePeriodMs / 1000));
        const result = await this.run(["stop", "--time", String(graceSeconds), name]);
        if (result.code === 0) {
          spawned.kill();
          return false;
        }

        await this.run(["rm", "--force", name]);
        spawned.kill();
        this.log.warn("container did not stop and was killed", {
          session: launch.sessionId,
          name,
        });
        return true;
      },
    });
  }

  async listOrphans(): Promise<string[]> {
    const result = await this.run([
      "ps",
      "--all",
      "--filter",
      `label=${SYSTEM_LABEL}=true`,
      "--format",
      "{{.Names}}",
    ]);
    if (result.code !== 0) {
      throw new SandboxLaunchError(`could not list containers: ${result.stderr.trim()}`);
    }
    return result.stdout
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
  }

  async removeOrphans(names: readonly string[]): Promise<number> {
    let removed = 0;
    for (const name of names) {
      const result = await this.run(["rm", "--force", name]);
      if (result.code === 0) removed += 1;
      else this.log.warn("could not remove a leftover container", { name });
    }
    return removed;
  }
}
