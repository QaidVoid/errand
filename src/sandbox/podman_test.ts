import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { DEFAULTS, type SandboxConfig } from "../config/schema.ts";
import { parseSize } from "../config/size.ts";
import { createLogger } from "../log.ts";
import { type SandboxLaunch, SandboxUnavailableError, SYSTEM_LABEL } from "./backend.ts";
import {
  FORBIDDEN_ARGS,
  podmanArgs,
  PodmanSandbox,
  RESTRICTED_NETWORK,
  type Run,
} from "./podman.ts";

const CONFIG: SandboxConfig = {
  ...DEFAULTS.sandbox,
  backend: "podman",
  policyExtra: undefined,
  pathExtra: undefined,
  env: undefined,
};

function launch(overrides: Partial<SandboxLaunch> = {}): SandboxLaunch {
  return {
    sessionId: "s-1",
    projectPath: "/projects/demo",
    stateDir: "/state/s-1",
    env: { ZAI_API_KEY: "provider-secret" },
    systemPromptPath: undefined,
    provider: "zai-coding-cn",
    model: "glm-5.3",
    resume: false,
    ...overrides,
  };
}

function fakeRun(answers: Record<string, { code?: number; stdout?: string }> = {}) {
  const calls: string[][] = [];
  const run: Run = (args) => {
    calls.push([...args]);
    const key = args[0] ?? "";
    const answer = answers[key] ?? {};
    return Promise.resolve({
      code: answer.code ?? 0,
      stdout: answer.stdout ?? (key === "info" ? "true" : ""),
      stderr: "",
    });
  };
  return { run, calls };
}

/** Each of these would undo the isolation this backend exists to provide. */
Deno.test("nothing that would open the container is ever passed", () => {
  const args = podmanArgs(CONFIG, launch()).join(" ");

  for (const forbidden of FORBIDDEN_ARGS) {
    assertEquals(args.includes(forbidden), false, forbidden);
  }
});

Deno.test("the container drops privileges it never needs", () => {
  const args = podmanArgs(CONFIG, launch());

  assertEquals(args.includes("--cap-drop=ALL"), true);
  assertEquals(args.includes("--read-only"), true);
  assertEquals(args.includes("--userns=keep-id"), true);
  assertStringIncludes(args.join(" "), "--security-opt no-new-privileges");
});

/**
 * podman's default maps the host, from which a service bound to 0.0.0.0 is
 * reachable from inside the container.
 */
Deno.test("the host is not reachable from a session with network", () => {
  const args = podmanArgs(CONFIG, launch()).join(" ");

  assertStringIncludes(args, `--network=${RESTRICTED_NETWORK}`);
  assertStringIncludes(args, "--map-host-loopback,none");
  assertStringIncludes(args, "--map-guest-addr,none");
});

Deno.test("a session with no network is given none at all", () => {
  const args = podmanArgs({ ...CONFIG, network: "none" }, launch()).join(" ");

  assertStringIncludes(args, "--network=none");
  assertEquals(args.includes("pasta"), false);
});

Deno.test("the project and the state are the only writable mounts", () => {
  const args = podmanArgs(CONFIG, launch());
  const volumes = args.filter((_arg, index) => args[index - 1] === "--volume");

  assertEquals(volumes, [
    "/projects/demo:/workspace:rw,Z",
    "/state/s-1:/state:rw,Z",
  ]);
});

Deno.test("the configured limits reach the container", () => {
  const args = podmanArgs({ ...CONFIG, memory: "2g", cpus: 1, pids: 128 }, launch()).join(" ");

  assertStringIncludes(args, "--memory 2g");
  assertStringIncludes(args, "--cpus 1");
  assertStringIncludes(args, "--pids-limit 128");
});

/** The runtime takes bytes, so a size that validated must convert. */
Deno.test("the file ceiling is passed as bytes, not as it was written", () => {
  const args = podmanArgs({ ...CONFIG, fileMax: "512m" }, launch()).join(" ");

  assertStringIncludes(args, `--ulimit fsize=${parseSize("512m")}`);
  assertEquals(args.includes("fsize=512m"), false);
});

Deno.test("the container is labelled so a leftover can be found later", () => {
  const args = podmanArgs(CONFIG, launch()).join(" ");

  assertStringIncludes(args, `${SYSTEM_LABEL}=true`);
  assertStringIncludes(args, "errand.session=s-1");
  assertStringIncludes(args, "--name errand-s-1");
});

Deno.test("the agent is started with its provider and model, inside the image", () => {
  const args = podmanArgs(CONFIG, launch());
  const image = args.indexOf(CONFIG.image);

  assertEquals(image > 0, true, "the image leads the command");
  assertStringIncludes(args.slice(image).join(" "), "pi --mode rpc --session-dir /state/sessions");
  assertStringIncludes(args.join(" "), "--provider zai-coding-cn");
});

Deno.test("the credential crosses as an environment variable", () => {
  const args = podmanArgs(CONFIG, launch()).join(" ");
  assertStringIncludes(args, "--env ZAI_API_KEY=provider-secret");
  assertStringIncludes(args, "--env HOME=/state/home");
});

Deno.test("variables set by configuration cross into the container", () => {
  const config = { ...CONFIG, env: { CARGO_HOME: "/var/cache/cargo" } };
  assertStringIncludes(
    podmanArgs(config, launch()).join(" "),
    "--env CARGO_HOME=/var/cache/cargo",
  );
});

/** The credential is plumbing, so a file cannot decide what the agent uses. */
Deno.test("a variable the daemon sets keeps the daemon's value", () => {
  const config = { ...CONFIG, env: { ZAI_API_KEY: "not-the-real-one" } };
  const args = podmanArgs(config, launch());

  assertEquals(
    args.lastIndexOf("ZAI_API_KEY=provider-secret") >
      args.lastIndexOf("ZAI_API_KEY=not-the-real-one"),
    true,
  );
});

Deno.test("podman that is not rootless cannot run this backend", async () => {
  const { run } = fakeRun({ info: { stdout: "false" } });
  const sandbox = new PodmanSandbox(CONFIG, createLogger({}, () => {}), run);

  const error = await assertRejects(() => sandbox.probe(), SandboxUnavailableError);
  assertStringIncludes(String(error), "not running rootless");
});

Deno.test("a missing image is reported at startup, not at the first session", async () => {
  const { run } = fakeRun({ "image": { code: 1 } });
  const sandbox = new PodmanSandbox(CONFIG, createLogger({}, () => {}), run);

  const error = await assertRejects(() => sandbox.probe(), SandboxUnavailableError);
  assertStringIncludes(String(error), "is not present");
});

Deno.test("a healthy host reports what it enforces and no gaps", async () => {
  const { run } = fakeRun();
  const sandbox = new PodmanSandbox(CONFIG, createLogger({}, () => {}), run);

  const report = await sandbox.probe();

  assertEquals(report.gaps, []);
  assertStringIncludes(report.notes.join("\n"), "rootless containers");
  assertStringIncludes(report.notes.join("\n"), "measured rather than enforced");
});

Deno.test("leftover containers are found by label and removed", async () => {
  const { run, calls } = fakeRun({ ps: { stdout: "errand-s-1\nerrand-s-2\n" } });
  const sandbox = new PodmanSandbox(CONFIG, createLogger({}, () => {}), run);

  assertEquals(await sandbox.listOrphans(), ["errand-s-1", "errand-s-2"]);
  assertEquals(await sandbox.removeOrphans(["errand-s-1", "errand-s-2"]), 2);

  const filter = calls.find((call) => call[0] === "ps")?.join(" ") ?? "";
  assertStringIncludes(filter, `label=${SYSTEM_LABEL}=true`);
});
