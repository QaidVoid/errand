import { assertEquals, assertStringIncludes } from "@std/assert";
import type { SandboxLaunch } from "./backend.ts";
import { policyContents, policyPath, RESOLV_CONF } from "./policy.ts";
import type { AgentRuntime } from "./runtime.ts";

const RUNTIME: AgentRuntime = {
  readPaths: ["/opt/agent/bin", "/opt/agent/lib/pi"],
  pathEntries: ["/opt/agent/bin"],
};

function launch(): SandboxLaunch {
  return {
    sessionId: "s-1",
    projectPath: "/home/operator/code/demo",
    stateDir: "/home/operator/.local/state/errand/s-1",
    env: { ZAI_API_KEY: "secret-value" },
    systemPromptPath: undefined,
    provider: "zai-coding-cn",
    model: "glm-5.3",
    resume: false,
  };
}

function policy(overrides: Record<string, unknown> = {}): string {
  return policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/var/lib/errand/resolv.conf",
    ...overrides,
  });
}

Deno.test("the policy clears the profile's grants before listing its own", () => {
  assertStringIncludes(policy(), "reset = true");
});

/** A host path names the operator and the shape of their machine. */
Deno.test("the project and the state are placed, never shown as host paths", () => {
  const written = policy();

  assertStringIncludes(written, '{ path = "/home/operator/code/demo", at = "/workspace" }');
  assertStringIncludes(
    written,
    '{ path = "/home/operator/.local/state/errand/s-1", at = "/state" }',
  );
});

Deno.test("the state directory is readable as well as writable", () => {
  const written = policy();
  const read = written.split("\n").find((line) => line.startsWith("read = ")) ?? "";
  const write = written.split("\n").find((line) => line.startsWith("write = ")) ?? "";

  assertStringIncludes(read, 'at = "/state"');
  assertStringIncludes(write, 'at = "/state"');
});

/** Only the project and the session's own state may be written. */
Deno.test("nothing outside the session is writable", () => {
  const write = policy().split("\n").find((line) => line.startsWith("write = ")) ?? "";

  assertEquals(write.includes("/usr"), false);
  assertEquals(write.includes("/etc"), false);
  assertEquals(write.includes("/proc"), false);
  assertEquals((write.match(/path = /g) ?? []).length, 2);
});

Deno.test("the host's own resolver is never granted", () => {
  const written = policy();

  assertStringIncludes(
    written,
    '{ path = "/var/lib/errand/resolv.conf", at = "/etc/resolv.conf" }',
  );
  assertEquals(written.includes('"/etc/resolv.conf"]'), false);
  assertEquals(written.includes('"/etc/resolv.conf",'), false);
});

Deno.test("the resolver that is handed over names a public one, not the host's", () => {
  assertStringIncludes(RESOLV_CONF, "nameserver 1.1.1.1");
  assertEquals(RESOLV_CONF.includes("192.168."), false);
});

/**
 * The daemon's environment holds the chat token. Naming what crosses is the
 * boundary that keeps it out of a session.
 */
Deno.test("only the named variables cross, and no value is written", () => {
  const written = policy({
    launch: { ...launch(), env: { ZAI_API_KEY: "secret-value", GH_TOKEN: "another-secret" } },
  });

  assertStringIncludes(written, 'pass = ["GH_TOKEN", "ZAI_API_KEY"]');
  assertEquals(written.includes("secret-value"), false);
  assertEquals(written.includes("another-secret"), false);
});

Deno.test("the agent's own directories are readable, or it cannot start", () => {
  const written = policy();

  assertStringIncludes(written, '"/opt/agent/lib/pi"');
  assertStringIncludes(written, "/opt/agent/bin");
});

Deno.test("the wrapper directory leads the path and is executable", () => {
  const written = policy();
  const path = written.split("\n").find((line) => line.startsWith("set = ")) ?? "";
  const execute = written.split("\n").find((line) => line.startsWith("execute = ")) ?? "";

  assertStringIncludes(
    path,
    'PATH = "/state/home/bin:/opt/agent/bin:/usr/local/bin:/usr/bin:/bin"',
  );
  assertStringIncludes(path, 'HOME = "/state/home"');
  assertStringIncludes(execute, '"/state/home/bin"');
});

Deno.test("a file size ceiling is set as a resource limit", () => {
  assertStringIncludes(policy({ fileMax: "512m" }), 'file_max = "512m"');
});

Deno.test("outbound https is allowed, and no network means no egress at all", () => {
  assertStringIncludes(policy(), 'egress_allow = [{ host = "*", port = 443 }]');

  const offline = policy({ network: "none" });
  assertEquals(offline.includes("[network]"), false);
  assertEquals(offline.includes("egress_allow"), false);
});

Deno.test("the policy lives in the state directory, never in the project", () => {
  const written = policyPath(launch());

  assertStringIncludes(written, "/home/operator/.local/state/errand/s-1/");
  assertEquals(written.startsWith("/home/operator/code/demo"), false);
});
