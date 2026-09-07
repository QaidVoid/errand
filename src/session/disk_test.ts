import { assertEquals } from "@std/assert";
import { MIN_CHECK_MS, nextCheckMs, treeBytes, verdict } from "./disk.ts";

Deno.test("everything under a directory is counted, at any depth", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-disk-" });
  await Deno.writeTextFile(`${root}/a.txt`, "x".repeat(100));
  await Deno.mkdir(`${root}/deep/deeper`, { recursive: true });
  await Deno.writeTextFile(`${root}/deep/b.txt`, "y".repeat(50));
  await Deno.writeTextFile(`${root}/deep/deeper/c.txt`, "z".repeat(25));

  assertEquals(await treeBytes(root), 175);
  await Deno.remove(root, { recursive: true });
});

/** Following one would let a session look enormous, or hide what it wrote. */
Deno.test("a symlink counts as the link, not as what it points at", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-disk-" });
  await Deno.writeTextFile(`${root}/real.txt`, "x".repeat(1000));
  await Deno.mkdir(`${root}/inside`);
  await Deno.symlink(`${root}/real.txt`, `${root}/inside/link.txt`);

  const total = (await treeBytes(`${root}/inside`)) ?? 0;

  assertEquals(total < 1000, true, "the link is not counted as its target");
  await Deno.remove(root, { recursive: true });
});

Deno.test("a directory that is not there is not zero", async () => {
  assertEquals(await treeBytes("/no/such/place/at/all"), undefined);
});

Deno.test("an empty directory holds nothing", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-disk-" });
  assertEquals(await treeBytes(root), 0);
  await Deno.remove(root, { recursive: true });
});

Deno.test("a session under its budget is left alone, and over it is not", () => {
  assertEquals(verdict(0, 1_000), "under");
  assertEquals(verdict(700, 1_000), "under");
  assertEquals(verdict(800, 1_000), "close");
  assertEquals(verdict(1_000, 1_000), "over");
  assertEquals(verdict(5_000, 1_000), "over");
});

/** No budget is not a budget of zero, which everything would be over. */
Deno.test("a session with no budget is always under it", () => {
  assertEquals(verdict(9_999, 0), "under");
});

Deno.test("an idle session settles back to the configured interval", () => {
  assertEquals(nextCheckMs(500, 500, 10_000, 1_000, 30_000), 30_000);
  assertEquals(nextCheckMs(400, 500, 10_000, 1_000, 30_000), 30_000);
});

/**
 * A fixed interval decides the overshoot: at 30 second checks, a session
 * writing a gigabyte a second is 20 GB past a 5 GB budget before anything
 * notices. That is not hypothetical, it happened.
 */
Deno.test("a fast writer is measured again long before it reaches its budget", () => {
  const budget = 5_000_000_000;
  const written = 1_000_000_000;

  const next = nextCheckMs(written, 0, budget, 1_000, 30_000);

  assertEquals(next, 2_000);
  assertEquals(next < 30_000, true);
});

Deno.test("the check never runs faster than its floor", () => {
  assertEquals(nextCheckMs(999, 0, 1_000, 1_000, 30_000), MIN_CHECK_MS);
});
