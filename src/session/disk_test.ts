import { assertEquals } from "@std/assert";
import { treeBytes } from "./disk.ts";

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
