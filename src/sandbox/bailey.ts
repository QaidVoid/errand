/**
 * Confines a session with Landlock and seccomp, using no container.
 *
 * The agent runs as a host process with its own namespaces and a filesystem
 * policy that names everything it may touch. What it may reach is bounded by
 * the generated policy rather than by an image, which is why the policy is a
 * module of its own.
 */

import { join } from "@std/path";
import type { SandboxConfig } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import {
  AGENT_SESSIONS,
  agentCommand,
  type CapabilityReport,
  type Sandbox,
  type SandboxHandle,
  type SandboxLaunch,
  SandboxLaunchError,
  sandboxName,
  SandboxUnavailableError,
  STATE_PATH,
  WORKSPACE_PATH,
} from "./backend.ts";
import { hostPathUnder } from "./paths.ts";
import {
  AGENT_PROFILE,
  OFFLINE_PROFILE,
  policyContents,
  policyPath,
  RESOLV_CONF,
  RESOLV_FILENAME,
} from "./policy.ts";
import { agentRuntime } from "./runtime.ts";
import { spawnAgent } from "./spawn.ts";

/** What the tool prints when it read a policy and then ignored it. */
const NOT_APPLYING = "not applying";

/** Variables a confined process needs in order to run at all. */
/**
 * What crosses from the daemon's environment into the sandbox tool's.
 *
 * `BAILEY_CGROUP_ROOT` names a cgroup the tool may create children in, which
 * is the only way per-session memory, cpu, and process limits are applied at
 * all. It is a path rather than a secret, and without it here an operator can
 * set it on the service and watch it have no effect.
 */
const INHERITED_VARIABLES = ["PATH", "LANG", "LC_ALL", "TERM", "BAILEY_CGROUP_ROOT"];

/** Runs the sandbox tool and collects what it said. Injected for tests. */
export type Run = (
  args: readonly string[],
  cwd?: string,
) => Promise<{ code: number; stdout: string; stderr: string }>;

const runBailey: Run = async (args, cwd) => {
  const command = new Deno.Command("bailey", {
    args: [...args],
    stdout: "piped",
    stderr: "piped",
    ...(cwd === undefined ? {} : { cwd }),
  });
  const { code, stdout, stderr } = await command.output();
  const decoder = new TextDecoder();
  return { code, stdout: decoder.decode(stdout), stderr: decoder.decode(stderr) };
};

/**
 * The environment a session runs with.
 *
 * Rebuilt from a named list rather than inherited. The daemon's own
 * environment holds the chat token, and inheriting it wholesale would put that
 * token inside the sandbox.
 */
export function sessionEnvironment(
  launchEnv: Record<string, string>,
  source: Record<string, string | undefined>,
  home: string,
): Record<string, string> {
  const env: Record<string, string> = {};
  for (const name of INHERITED_VARIABLES) {
    const value = source[name];
    if (value !== undefined) env[name] = value;
  }
  // The tool builds its own world from the caller's HOME and must be able to
  // create it, so it is given a host path. The target's HOME is set in the
  // policy instead, to the placed path, which overrides this.
  env.HOME = home;
  return { ...env, ...launchEnv };
}

/**
 * Reads gaps out of what the tool reports about this host.
 *
 * Parsed rather than hardcoded, so this does not drift from what the tool
 * actually does.
 */
export function parseDoctor(doctor: string): { gaps: string[]; unavailable: string[] } {
  const gaps: string[] = [];
  const unavailable: string[] = [];
  const lines = doctor.split("\n").map((line) => line.trim());

  const landlock = lines.find((line) => line.startsWith("landlock:"));
  if (landlock === undefined || landlock.includes("no")) {
    unavailable.push("the kernel does not provide Landlock, which this backend requires");
  }

  const userns = lines.find((line) => line.startsWith("user namespaces:"));
  if (userns?.endsWith("no") === true) {
    unavailable.push("the kernel does not allow user namespaces, which this backend requires");
  }

  const cgroups = lines.find((line) => line.startsWith("cgroup delegation:"));
  if (cgroups?.endsWith("no") === true) {
    gaps.push(
      "per-session memory, cpu, and process limits are not applied: this host reports no cgroup delegation. A limit on the daemon as a whole still applies to it and every session together",
    );
  }

  return { gaps, unavailable };
}

/** The arguments the tool is run with for one session. */
export function baileyArgs(
  config: SandboxConfig,
  launch: SandboxLaunch,
  policy: string,
): string[] {
  return [
    "run",
    "--isolate",
    "--config",
    policy,
    "--profile",
    config.network === "none" ? OFFLINE_PROFILE : AGENT_PROFILE,
    "--",
    ...agentCommand({
      sessionDir: AGENT_SESSIONS,
      provider: launch.provider,
      model: launch.model,
      // Written into the state directory by the daemon, so the agent reaches
      // it where the state is placed rather than where the host keeps it.
      systemPromptPath: launch.systemPromptPath === undefined
        ? undefined
        : `${STATE_PATH}/${launch.systemPromptPath.split("/").pop()}`,
      resume: launch.resume,
    }),
  ];
}

/** Confines sessions as host processes. */
export class BaileySandbox implements Sandbox {
  readonly name = "bailey" as const;
  private readonly live = new Map<string, () => void>();

  constructor(
    private readonly config: SandboxConfig,
    private readonly log: Logger,
    private readonly stateRoot: string,
    private readonly run: Run = runBailey,
  ) {}

  async probe(): Promise<CapabilityReport> {
    const doctor = await this.run(["doctor"]).catch(() => null);
    if (doctor === null || doctor.code !== 0) {
      throw new SandboxUnavailableError("bailey", [
        "bailey is not installed, or `bailey doctor` failed",
      ]);
    }

    const { gaps, unavailable } = parseDoctor(`${doctor.stdout}\n${doctor.stderr}`);
    if (unavailable.length > 0) throw new SandboxUnavailableError("bailey", unavailable);

    // Without this, a version too old for the generated policy surfaces as
    // every session failing to launch rather than once at startup, where it is
    // actionable.
    if (!(await this.acceptsGeneratedPolicy())) {
      throw new SandboxUnavailableError("bailey", [
        "the installed bailey does not accept the policy this backend writes, which needs resources.file_max and relocatable grants; update bailey",
      ]);
    }

    // This backend runs the host's own agent rather than one baked into an
    // image, so an agent that is not installed is a reason to refuse to start.
    if (agentRuntime() === undefined) {
      throw new SandboxUnavailableError("bailey", [
        "the pi agent is not on PATH, and this backend runs the host's own installation",
      ]);
    }

    const notes = [
      "sessions run as confined host processes using the host's own tools",
      `no single file may exceed ${this.config.fileMax}, enforced as an rlimit and so holding with or without cgroups`,
      // A note rather than a gap: the daemon never claims to enforce a disk
      // total, so calling it an unenforceable guarantee would make
      // requireFullEnforcement refuse to start on every host forever.
      `a session is stopped once it has written ${this.config.disk}, which is measured rather than enforced`,
      this.config.network === "none"
        ? "sessions have no network, so the agent cannot reach a model provider"
        : "sessions reach the model provider over TCP 443, and outbound access is not restricted by destination",
    ];

    // Said out loud, so the report never describes a tighter boundary than the
    // one actually applied. Write is named separately: it is the grant that
    // lets a session change something outside its own project.
    const extra = this.config.policyExtra;
    if (extra !== undefined) {
      const granted = extra.read.length + extra.write.length + extra.execute.length;
      notes.push(
        `sandbox.policyExtra grants ${granted} path(s) beyond the generated policy`,
      );
      if (extra.write.length > 0) {
        notes.push(
          `  ${extra.write.length} of them writable, so a session can change what is outside its project: ${
            extra.write.join(", ")
          }`,
        );
      }
    }

    return { backend: "bailey", gaps, notes };
  }

  async launch(launch: SandboxLaunch): Promise<SandboxHandle> {
    const runtime = agentRuntime();
    if (runtime === undefined) {
      throw new SandboxLaunchError(
        "the pi agent is not on PATH, so there is nothing for a confined session to run",
      );
    }

    await Deno.mkdir(launch.stateDir, { recursive: true });
    const resolv = await this.writeResolvConf();
    const policy = policyPath(launch);
    await Deno.writeTextFile(
      policy,
      policyContents({
        launch,
        network: this.config.network,
        runtime,
        fileMax: this.config.fileMax,
        resolvConf: resolv,
        extra: this.config.policyExtra,
      }),
    );

    const trusted = await this.run(["trust", policy]);
    if (trusted.code !== 0) {
      throw new SandboxLaunchError(
        `could not trust the generated policy at ${policy}: ${trusted.stderr.trim()}`,
      );
    }
    await this.verifyPolicyApplies(policy);

    // Started from the project, so the tool enters it after the pivot. It is
    // the agent's working directory, and without this the agent starts in a
    // private home rather than the project it was asked to work in.
    const spawned = spawnAgent(
      "bailey",
      baileyArgs(this.config, launch, policy),
      sessionEnvironment(launch.env, Deno.env.toObject(), join(launch.stateDir, "home")),
      launch.projectPath,
    );
    const name = sandboxName(launch.sessionId);

    let stopped = false;
    const stop = async (): Promise<boolean> => {
      if (stopped) return false;
      stopped = true;
      this.live.delete(name);

      spawned.kill("SIGTERM");
      const exitedInTime = await Promise.race([
        spawned.process.exited.then(() => true),
        new Promise<boolean>((resolve) =>
          setTimeout(() => resolve(false), this.config.gracePeriodMs)
        ),
      ]);

      let killed = false;
      if (!exitedInTime) {
        spawned.kill("SIGKILL");
        killed = true;
        this.log.warn("confined process did not stop and was killed", {
          session: launch.sessionId,
          name,
        });
      }

      await this.run(["untrust", policy]).catch(() => undefined);
      return killed;
    };

    this.live.set(name, () => spawned.kill());
    this.log.info("confined process started", {
      session: launch.sessionId,
      name,
      pid: spawned.pid,
    });

    return {
      process: spawned.process,
      name,
      toHostPath: (agentPath: string) =>
        hostPathUnder(WORKSPACE_PATH, launch.projectPath, agentPath),
      stop,
    };
  }

  /**
   * The tool runs inside a PID namespace, so killing the launcher removes
   * every process the agent started. There is nothing to reap afterwards, and
   * a previous daemon's processes died with it.
   */
  listOrphans(): Promise<string[]> {
    return Promise.resolve([]);
  }

  removeOrphans(names: readonly string[]): Promise<number> {
    for (const name of names) this.live.get(name)?.();
    return Promise.resolve(0);
  }

  /**
   * Writes the resolver a session is given, and returns its path.
   *
   * Rewritten on every launch rather than once, so a daemon whose idea of the
   * resolver changed does not keep handing out the file it wrote first.
   */
  private async writeResolvConf(): Promise<string> {
    await Deno.mkdir(this.stateRoot, { recursive: true });
    const path = join(this.stateRoot, RESOLV_FILENAME);
    await Deno.writeTextFile(path, `${RESOLV_CONF}\n`);
    return path;
  }

  /**
   * Whether the installed tool understands the policy this backend writes.
   *
   * It rejects a config holding a key it does not know, so a version older
   * than a feature used here fails every launch. Checking the shape rather
   * than one key means a later addition is covered by the same check.
   */
  private async acceptsGeneratedPolicy(): Promise<boolean> {
    const probeDir = join(this.stateRoot, "probe");
    await Deno.mkdir(probeDir, { recursive: true });
    const policy = join(probeDir, "shape.toml");
    await Deno.writeTextFile(
      policy,
      policyContents({
        launch: {
          sessionId: "probe",
          projectPath: probeDir,
          stateDir: probeDir,
          env: {},
          provider: "probe",
          model: undefined,
          systemPromptPath: undefined,
          resume: false,
        },
        network: this.config.network,
        // The probe runs `true`, so it needs nothing of the agent granted.
        runtime: { readPaths: [], pathEntries: [] },
        fileMax: this.config.fileMax,
        resolvConf: join(probeDir, RESOLV_FILENAME),
      }),
    );
    await Deno.writeTextFile(join(probeDir, RESOLV_FILENAME), `${RESOLV_CONF}\n`);

    await this.run(["trust", policy]);
    const result = await this.run(["run", "--config", policy, "--quiet", "--", "true"], probeDir);
    await this.run(["untrust", policy]).catch(() => undefined);
    return result.code === 0;
  }

  /**
   * Confirms the policy will be applied before a session is started.
   *
   * The tool does not fail a run whose policy it declined to trust. It warns
   * and continues without the policy, which would start a session with no
   * project grant at all. A cheap confined command is run first so that case
   * becomes a launch failure rather than a silently unconfined session.
   */
  private async verifyPolicyApplies(policy: string): Promise<void> {
    const check = await this.run([
      "run",
      "--isolate",
      "--config",
      policy,
      "--profile",
      this.config.network === "none" ? OFFLINE_PROFILE : AGENT_PROFILE,
      "--quiet",
      "--",
      "true",
    ]);
    if (check.stderr.includes(NOT_APPLYING)) {
      throw new SandboxLaunchError(
        `bailey declined to apply the generated policy at ${policy}, which would leave the session without its project grant: ${check.stderr.trim()}`,
      );
    }
  }
}
