import { assertEquals, assertThrows } from "@std/assert";
import { join } from "@std/path";
import { acquireLock, AlreadyRunningError, isRunning, LOCK_FILENAME } from "./lock.ts";

async function withStateDir(run: (stateDir: string) => void | Promise<void>): Promise<void> {
  const stateDir = await Deno.makeTempDir({ prefix: "errand-lock-" });
  try {
    await run(stateDir);
  } finally {
    await Deno.remove(stateDir, { recursive: true });
  }
}

Deno.test("taking the lock writes the holder's process id", () =>
  withStateDir((stateDir) => {
    const lock = acquireLock(stateDir, 4242);

    assertEquals(lock.path, join(stateDir, LOCK_FILENAME));
    assertEquals(Deno.readTextFileSync(lock.path).trim(), "4242");
    lock.release();
  }));

/** Two daemons on one token both act on every message. */
Deno.test("a second daemon is refused while the first is alive", () =>
  withStateDir((stateDir) => {
    const first = acquireLock(stateDir, Deno.pid);

    const error = assertThrows(
      () => acquireLock(stateDir, 9999),
      AlreadyRunningError,
    ) as AlreadyRunningError;

    assertEquals(error.pid, Deno.pid);
    assertEquals(error.message.includes(stateDir), true);
    first.release();
  }));

/** A killed daemon must not block every restart after it. */
Deno.test("a lock left by a process that is gone is taken over", () =>
  withStateDir((stateDir) => {
    Deno.writeTextFileSync(join(stateDir, LOCK_FILENAME), "999999999\n");

    const lock = acquireLock(stateDir, 4242);

    assertEquals(Deno.readTextFileSync(lock.path).trim(), "4242");
    lock.release();
  }));

Deno.test("a lock holding nonsense is treated as stale", () =>
  withStateDir((stateDir) => {
    Deno.writeTextFileSync(join(stateDir, LOCK_FILENAME), "not a pid\n");

    const lock = acquireLock(stateDir, 4242);

    assertEquals(Deno.readTextFileSync(lock.path).trim(), "4242");
    lock.release();
  }));

Deno.test("releasing removes the lock, and twice is not an error", () =>
  withStateDir((stateDir) => {
    const lock = acquireLock(stateDir, 4242);

    lock.release();
    lock.release();

    assertThrows(() => Deno.statSync(lock.path));
    acquireLock(stateDir, 4243).release();
  }));

Deno.test("a process is running when it is, and not when it is not", () => {
  assertEquals(isRunning(Deno.pid), true);
  assertEquals(isRunning(999_999_999), false);
  assertEquals(isRunning(0), false);
  assertEquals(isRunning(-1), false);
  assertEquals(isRunning(Number.NaN), false);
});
