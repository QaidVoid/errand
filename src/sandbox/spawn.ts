/**
 * Starting a confined launcher and wiring up its pipes.
 *
 * The launcher leads its own process group, so ending a session ends
 * everything the agent started rather than only the launcher, leaving its
 * children behind holding the project open.
 */

import type { AgentProcess } from "../agent/client.ts";

/** A launcher that has been started. */
export interface SpawnedAgent {
  process: AgentProcess;
  /** Terminates the whole process group. Safe to call more than once. */
  kill(signal?: Deno.Signal): void;
  /** The launcher's process id, which is also its process group id. */
  pid: number;
}

/** Spawns a launcher in its own process group and wires up its pipes. */
export function spawnAgent(
  command: string,
  args: readonly string[],
  env?: Record<string, string>,
  cwd?: string,
): SpawnedAgent {
  // Through setsid, so the launcher leads a group and one signal reaches
  // everything it started.
  const child = new Deno.Command("setsid", {
    args: [command, ...args],
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
    ...(env === undefined ? {} : { env, clearEnv: true }),
    ...(cwd === undefined ? {} : { cwd }),
  }).spawn();

  let killed = false;
  const writer = child.stdin.getWriter();

  const process: AgentProcess = {
    stdout: child.stdout,
    stderr: child.stderr,
    write(bytes: Uint8Array): void {
      if (killed) throw new Error("the sandbox launcher has been terminated");
      // Not awaited: the protocol is a stream of whole lines and the caller
      // has nothing to do with the acknowledgement. A failed write surfaces
      // as the process ending, which the client already handles.
      void writer.write(bytes);
    },
    exited: child.status.then((status) => status.code),
  };

  return {
    process,
    pid: child.pid,
    kill(signal: Deno.Signal = "SIGKILL"): void {
      killed = true;
      try {
        // The negative pid is the group, which is the point of setsid.
        Deno.kill(-child.pid, signal);
      } catch {
        // The group is already gone, or the process never made it far enough
        // to lead one. Fall back to the launcher itself.
        try {
          child.kill(signal);
        } catch {
          // Already gone, which is the state we wanted.
        }
      }
    },
  };
}
