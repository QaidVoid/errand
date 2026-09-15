import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import { agentRuntime } from "./runtime.ts";

/**
 * The agent's dependencies are packages of their own, beside it rather than
 * inside it. Granting only its own package leaves an import of a sibling
 * unresolvable, which surfaces as the agent exiting at startup.
 */
Deno.test("the node_modules the agent was installed into is granted", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-runtime-" });
  try {
    const modules = join(root, "node_modules");
    const own = join(modules, "@scope", "agent");
    const sibling = join(modules, "@scope", "helper");
    Deno.mkdirSync(join(own, "bin"), { recursive: true });
    Deno.mkdirSync(sibling, { recursive: true });
    Deno.writeTextFileSync(join(own, "package.json"), "{}");
    Deno.writeTextFileSync(join(sibling, "package.json"), "{}");
    const launcher = join(own, "bin", "pi");
    Deno.writeTextFileSync(launcher, "#!/usr/bin/env node\n");

    const runtime = agentRuntime((name) => (name === "pi" ? launcher : undefined));

    // The package itself, and the tree its siblings resolve from.
    assertEquals(runtime?.readPaths.includes(own), true);
    assertEquals(runtime?.readPaths.includes(modules), true);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});

Deno.test("an agent outside any node_modules grants only what it has", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-runtime-" });
  try {
    const own = join(root, "opt", "agent");
    Deno.mkdirSync(join(own, "bin"), { recursive: true });
    Deno.writeTextFileSync(join(own, "package.json"), "{}");
    const launcher = join(own, "bin", "pi");
    Deno.writeTextFileSync(launcher, "#!/usr/bin/env node\n");

    const runtime = agentRuntime((name) => (name === "pi" ? launcher : undefined));

    assertEquals(runtime?.readPaths.includes(own), true);
    // Nothing invented: there is no install tree to grant.
    assertEquals(runtime?.readPaths.some((path) => path.endsWith("node_modules")), false);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});
