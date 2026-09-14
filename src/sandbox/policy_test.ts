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

Deno.test("configured ports become the egress allowlist, in order", () => {
  assertStringIncludes(
    policy({ egressPorts: [80, 443] }),
    'egress_allow = [{ host = "*", port = 80 }, { host = "*", port = 443 }]',
  );
});

/** A session with no network opens nothing, whatever ports were named. */
Deno.test("ports do not grant egress to a session that has no network", () => {
  const offline = policy({ network: "none", egressPorts: [80, 443] });
  assertEquals(offline.includes("egress_allow"), false);
});

Deno.test("the policy lives in the state directory, never in the project", () => {
  const written = policyPath(launch());

  assertStringIncludes(written, "/home/operator/.local/state/errand/s-1/");
  assertEquals(written.startsWith("/home/operator/code/demo"), false);
});

const EXTRA = {
  read: ["/opt/toolchains", "/var/cache/shared"],
  write: ["/srv/output"],
  execute: ["/opt/toolchains/bin"],
};

Deno.test("paths granted by configuration reach the policy", () => {
  const policy = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    extra: EXTRA,
  });

  assertStringIncludes(policy, '"/opt/toolchains"');
  assertStringIncludes(policy, '"/var/cache/shared"');
  assertStringIncludes(policy, '"/srv/output"');
  assertStringIncludes(policy, '"/opt/toolchains/bin"');
});

/** Additive only: what the daemon grants is the floor, not a suggestion. */
Deno.test("an extra grant takes nothing away", () => {
  const plain = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
  });
  const widened = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    extra: EXTRA,
  });

  // Every path the generated policy names is still named in the widened one.
  for (const quoted of plain.match(/"[^"]+"/g) ?? []) {
    assertStringIncludes(widened, quoted);
  }
  assertStringIncludes(widened, "reset = true");
});

/** A grant that names nothing must not silently widen anything. */
Deno.test("granting nothing changes nothing", () => {
  const plain = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
  });
  const empty = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    extra: { read: [], write: [], execute: [] },
  });

  assertEquals(empty, plain);
});

Deno.test("variables set by configuration reach the policy", () => {
  const policy = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    env: { CARGO_HOME: "/var/cache/cargo" },
  });

  assertStringIncludes(policy, 'CARGO_HOME = "/var/cache/cargo"');
  assertStringIncludes(policy, 'HOME = "/state/home"');
});

/** The credential and the GitHub token are plumbing, not settings. */
Deno.test("a variable the daemon passes cannot be shadowed by configuration", () => {
  const policy = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    env: { ZAI_API_KEY: "not-the-real-one" },
  });

  assertEquals(policy.includes("not-the-real-one"), false);
});

/** A toolchain named on purpose is the one a session should find. */
Deno.test("directories added by configuration lead the system path", () => {
  const policy = policyContents({
    launch: launch(),
    network: "restricted",
    runtime: RUNTIME,
    fileMax: "1g",
    resolvConf: "/state/resolv.conf",
    pathExtra: ["/opt/toolchains/bin"],
  });

  const path = /PATH = "([^"]+)"/.exec(policy)?.[1]?.split(":") ?? [];

  assertEquals(path.indexOf("/opt/toolchains/bin") < path.indexOf("/usr/bin"), true);
  assertEquals(path[0], "/state/home/bin");
});

Deno.test("every family's certificate bundle is reachable, not just Debian's", () => {
  // Measured targets of the canonical bundle path: arch /etc/ca-certificates,
  // fedora /etc/pki, tumbleweed /var/lib/ca-certificates.
  const written = policy();

  for (
    const path of ["/etc/ssl", "/etc/ca-certificates", "/var/lib/ca-certificates", "/etc/pki"]
  ) {
    assertStringIncludes(written, `"${path}"`);
  }
});
