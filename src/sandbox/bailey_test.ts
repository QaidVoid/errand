import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { DEFAULTS, type SandboxConfig } from "../config/schema.ts";
import { createLogger } from "../log.ts";
import { type SandboxLaunch, SandboxUnavailableError } from "./backend.ts";
import { baileyArgs, BaileySandbox, parseDoctor, type Run, sessionEnvironment } from "./bailey.ts";

const CONFIG: SandboxConfig = {
  ...DEFAULTS.sandbox,
  policyExtra: undefined,
  pathExtra: undefined,
  env: undefined,
  egressPorts: [443],
};

function launch(overrides: Partial<SandboxLaunch> = {}): SandboxLaunch {
  return {
    sessionId: "s-1",
    projectPath: "/projects/demo",
    stateDir: "/state/s-1",
    env: {},
    systemPromptPath: undefined,
    provider: "zai-coding-cn",
    model: "glm-5.3",
    resume: false,
    ...overrides,
  };
}

const HEALTHY = [
  "landlock: yes (abi 5)",
  "user namespaces: yes",
  "cgroup delegation: yes",
  "seccomp: yes",
].join("\n");

/** Answers the tool's commands from a script, and records what was asked. */
function fakeRun(
  answers: Record<string, { code?: number; stdout?: string; stderr?: string }> = {},
) {
  const calls: string[][] = [];
  const run: Run = (args) => {
    calls.push([...args]);
    const key = args[0] ?? "";
    const answer = answers[key] ?? {};
    return Promise.resolve({
      code: answer.code ?? 0,
      stdout: answer.stdout ?? (key === "doctor" ? HEALTHY : ""),
      stderr: answer.stderr ?? "",
    });
  };
  return { run, calls };
}

Deno.test("a host missing landlock cannot run this backend at all", async () => {
  const { run } = fakeRun({ doctor: { stdout: "landlock: no\nuser namespaces: yes" } });
  const sandbox = new BaileySandbox(CONFIG, createLogger({}, () => {}), "/state", run);

  const error = await assertRejects(() => sandbox.probe(), SandboxUnavailableError);
  assertStringIncludes(String(error), "does not provide Landlock");
});

Deno.test("a host without user namespaces cannot run this backend either", () => {
  const { unavailable } = parseDoctor("landlock: yes\nuser namespaces: no");
  assertEquals(unavailable.length, 1);
  assertStringIncludes(unavailable[0] ?? "", "user namespaces");
});

/**
 * A gap, not a refusal: the daemon still runs, and says what it cannot
 * enforce, rather than pretending the limits are applied.
 */
Deno.test("no cgroup delegation is a gap that is reported, not a refusal", () => {
  const { gaps, unavailable } = parseDoctor(
    "landlock: yes\nuser namespaces: yes\ncgroup delegation: no",
  );

  assertEquals(unavailable, []);
  assertEquals(gaps.length, 1);
  assertStringIncludes(gaps[0] ?? "", "memory, cpu, and process limits are not applied");
});

Deno.test("a tool that is not installed is reported as unavailable", async () => {
  const run: Run = () => Promise.reject(new Error("no such command"));
  const sandbox = new BaileySandbox(CONFIG, createLogger({}, () => {}), "/state", run);

  const error = await assertRejects(() => sandbox.probe(), SandboxUnavailableError);
  assertStringIncludes(String(error), "not installed");
});

/**
 * A version too old for the generated policy would otherwise fail every
 * launch, rather than once at startup where it can be acted on.
 */
Deno.test("a tool that refuses the generated policy is caught at startup", async () => {
  const root = await Deno.makeTempDir();
  const { run } = fakeRun({ run: { code: 1, stderr: "unknown key: resources.file_max" } });
  const sandbox = new BaileySandbox(CONFIG, createLogger({}, () => {}), root, run);

  const error = await assertRejects(() => sandbox.probe(), SandboxUnavailableError);
  assertStringIncludes(String(error), "does not accept the policy");
  await Deno.remove(root, { recursive: true });
});

Deno.test("the probe writes its policy under the daemon's state, not the project", async () => {
  const root = await Deno.makeTempDir();
  const { run, calls } = fakeRun();
  const sandbox = new BaileySandbox(CONFIG, createLogger({}, () => {}), root, run);

  await sandbox.probe().catch(() => undefined);

  const trusted = calls.find((call) => call[0] === "trust")?.[1] ?? "";
  assertStringIncludes(trusted, root);
  await Deno.remove(root, { recursive: true });
});

Deno.test("the agent is started with its provider, model, and session directory", () => {
  const args = baileyArgs(CONFIG, launch(), "/state/s-1/policy.toml");

  assertEquals(args.slice(0, 6), [
    "run",
    "--isolate",
    "--config",
    "/state/s-1/policy.toml",
    "--profile",
    "ai-agent",
  ]);
  assertStringIncludes(args.join(" "), "pi --mode rpc --session-dir /state/sessions");
  assertStringIncludes(args.join(" "), "--provider zai-coding-cn");
  assertStringIncludes(args.join(" "), "--model glm-5.3");
  assertEquals(args.includes("--continue"), false);
});

Deno.test("hiding the host address asks the backend for a private namespace", () => {
  const plain = baileyArgs(CONFIG, launch(), "/p.toml");
  assertEquals(plain.includes("--proxy-net"), false);

  const hidden = baileyArgs({ ...CONFIG, hideHostAddress: true }, launch(), "/p.toml");
  assertStringIncludes(hidden.join(" "), "--isolate --proxy-net --config");
});

Deno.test("a session with no network runs under the offline profile", () => {
  const args = baileyArgs({ ...CONFIG, network: "none" }, launch(), "/p.toml");
  assertStringIncludes(args.join(" "), "--profile untrusted");
});

Deno.test("a resumed session continues the conversation it stored", () => {
  const args = baileyArgs(CONFIG, launch({ resume: true }), "/p.toml");
  assertEquals(args.includes("--continue"), true);
});

/** The agent reads it where the state is placed, not where the host keeps it. */
Deno.test("the system prompt is named at the path the agent will see", () => {
  const args = baileyArgs(
    CONFIG,
    launch({ systemPromptPath: "/home/operator/.local/state/errand/s-1/memory.md" }),
    "/p.toml",
  );

  assertStringIncludes(args.join(" "), "--append-system-prompt /state/memory.md");
  assertEquals(args.join(" ").includes("/home/operator"), false);
});

/** The daemon's own environment holds the chat token. */
Deno.test("the environment is rebuilt from a named list, not inherited", () => {
  const env = sessionEnvironment(
    { ZAI_API_KEY: "provider-secret" },
    { PATH: "/usr/bin", LANG: "en_GB.UTF-8", CHAT_TOKEN: "the-bot-token", HOME: "/home/operator" },
    "/state/s-1/home",
  );

  assertEquals(env.ZAI_API_KEY, "provider-secret");
  assertEquals(env.PATH, "/usr/bin");
  assertEquals(env.LANG, "en_GB.UTF-8");
  assertEquals(env.CHAT_TOKEN, undefined);
  assertEquals(env.HOME, "/state/s-1/home");
});

/**
 * Per-session limits are applied only when the tool has a cgroup it may
 * create children in, and it is told about one through this. Without it
 * crossing, an operator can set it on the service and watch it do nothing.
 */
Deno.test("the cgroup the tool may use is passed through", () => {
  const env = sessionEnvironment(
    {},
    { BAILEY_CGROUP_ROOT: "/sys/fs/cgroup/system.slice/errand.service", CHAT_TOKEN: "secret" },
    "/state/s-1/home",
  );

  assertEquals(env.BAILEY_CGROUP_ROOT, "/sys/fs/cgroup/system.slice/errand.service");
  assertEquals(env.CHAT_TOKEN, undefined);
});

/** The report must never describe a tighter boundary than the one applied. */
Deno.test("extra grants are named in what the backend reports", async () => {
  const root = await Deno.makeTempDir();
  const { run } = fakeRun();
  const sandbox = new BaileySandbox(
    {
      ...CONFIG,
      policyExtra: { read: ["/opt/toolchains"], write: ["/srv/output"], execute: [] },
    },
    createLogger({}, () => {}),
    root,
    run,
  );

  const report = await sandbox.probe();
  const said = report.notes.join("\n");

  assertStringIncludes(said, "grants 2 path(s) beyond the generated policy");
  assertStringIncludes(said, "1 of them writable");
  assertStringIncludes(said, "/srv/output");
  await Deno.remove(root, { recursive: true });
});

/** Names only: a value is the operator's own and may be anything. */
Deno.test("variables set by configuration are named in what the backend reports", async () => {
  const root = await Deno.makeTempDir();
  const { run } = fakeRun();
  const sandbox = new BaileySandbox(
    { ...CONFIG, env: { CARGO_HOME: "/var/cache/cargo" } },
    createLogger({}, () => {}),
    root,
    run,
  );

  const said = (await sandbox.probe()).notes.join("\n");

  assertStringIncludes(said, "sessions are given CARGO_HOME from configuration");
  assertEquals(said.includes("/var/cache/cargo"), false);
  await Deno.remove(root, { recursive: true });
});
