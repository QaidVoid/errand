import { assertEquals, assertStringIncludes } from "@std/assert";
import { checkBindAddress } from "./address.ts";

/** Reaching the interface is the authorisation, so the address is the control. */
Deno.test("loopback is allowed, by name or by address", () => {
  for (const host of ["127.0.0.1", "localhost", "::1", "[::1]", "127.0.0.53"]) {
    assertEquals(checkBindAddress(host).allowed, true, host);
  }
});

Deno.test("a private address is allowed, which is how a phone reaches it", () => {
  for (const host of ["10.0.0.5", "192.168.1.20", "172.16.4.1", "172.31.255.254", "169.254.1.1"]) {
    assertEquals(checkBindAddress(host).allowed, true, host);
  }
});

/** A tailnet lives in the shared address space, and that is the intended use. */
Deno.test("a tailnet address is allowed deliberately", () => {
  assertEquals(checkBindAddress("100.64.0.1").allowed, true);
  assertEquals(checkBindAddress("100.127.255.255").allowed, true);
});

Deno.test("a private IPv6 address is allowed", () => {
  for (const host of ["fd00::1", "[fd12:3456::1]", "fe80::1"]) {
    assertEquals(checkBindAddress(host).allowed, true, host);
  }
});

/** A warning in a log is not a control, so this is a refusal. */
Deno.test("a wildcard bind is refused, and says why", () => {
  for (const host of ["0.0.0.0", "::", "[::]", "*"]) {
    const verdict = checkBindAddress(host);
    assertEquals(verdict.allowed, false, host);
    assertStringIncludes(verdict.reason, "every interface");
  }
});

Deno.test("a public address is refused, and says the interface has no login", () => {
  for (const host of ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2606:4700::1111"]) {
    const verdict = checkBindAddress(host);
    assertEquals(verdict.allowed, false, host);
    assertStringIncludes(verdict.reason, "no login");
  }
});

/** Resolving here would make what is reachable depend on DNS at startup. */
Deno.test("a name that is not an address is refused as a name", () => {
  const verdict = checkBindAddress("errand.example.com");

  assertEquals(verdict.allowed, false);
  assertStringIncludes(verdict.reason, "is a name, not an address");
});

Deno.test("nothing at all is refused rather than treated as a default", () => {
  assertEquals(checkBindAddress("   ").allowed, false);
  assertEquals(checkBindAddress("").allowed, false);
});

/** An octet out of range is not an address, so it must not read as private. */
Deno.test("something that only looks like an address is not one", () => {
  for (const host of ["10.0.0.256", "10.0.0", "10.0.0.1.5", "010.0.0.1x"]) {
    assertEquals(checkBindAddress(host).allowed, false, host);
  }
});
