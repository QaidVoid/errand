import { assertEquals } from "@std/assert";
import {
  Broker,
  hostAllowed,
  isPrivateAddress,
  parseConnect,
  publicAddress,
  readRequestHead,
} from "./broker.ts";

const quiet = { info: () => {}, warn: () => {} };

Deno.test("a bare allow rule matches only itself", () => {
  assertEquals(hostAllowed("github.com", ["github.com"]), true);
  assertEquals(hostAllowed("GitHub.com", ["github.com"]), true);
  assertEquals(hostAllowed("evil.com", ["github.com"]), false);
  // A suffix that is not a subdomain must not match a bare rule.
  assertEquals(hostAllowed("notgithub.com", ["github.com"]), false);
  assertEquals(hostAllowed("github.com.evil.com", ["github.com"]), false);
});

Deno.test("a *. rule matches subdomains but not the apex", () => {
  assertEquals(hostAllowed("codeload.githubusercontent.com", ["*.githubusercontent.com"]), true);
  assertEquals(hostAllowed("a.b.githubusercontent.com", ["*.githubusercontent.com"]), true);
  // The apex is not a subdomain of itself; a wildcard is written when the apex
  // is not what is talked to.
  assertEquals(hostAllowed("githubusercontent.com", ["*.githubusercontent.com"]), false);
  // A lookalike that merely ends in the string but is not a subdomain.
  assertEquals(hostAllowed("evilgithubusercontent.com", ["*.githubusercontent.com"]), false);
});

Deno.test("an empty allowlist admits nothing", () => {
  assertEquals(hostAllowed("github.com", []), false);
  assertEquals(hostAllowed("", ["github.com"]), false);
});

Deno.test("a lone * admits any host but not the empty host", () => {
  assertEquals(hostAllowed("github.com", ["*"]), true);
  assertEquals(hostAllowed("derp1.tailscale.com", ["*"]), true);
  assertEquals(hostAllowed("anything.example", ["a.com", "*"]), true);
  // The catch-all still does not conjure a host out of nothing.
  assertEquals(hostAllowed("", ["*"]), false);
});

Deno.test("a CONNECT line yields its host and port, or nothing", () => {
  assertEquals(parseConnect("CONNECT github.com:443 HTTP/1.1"), { host: "github.com", port: 443 });
  assertEquals(parseConnect("connect github.com:443 HTTP/1.1"), { host: "github.com", port: 443 });
  // Not CONNECT, no port, junk port, out of range, and a smuggled path.
  assertEquals(parseConnect("GET / HTTP/1.1"), undefined);
  assertEquals(parseConnect("CONNECT github.com HTTP/1.1"), undefined);
  assertEquals(parseConnect("CONNECT github.com:https HTTP/1.1"), undefined);
  assertEquals(parseConnect("CONNECT github.com:99999 HTTP/1.1"), undefined);
  assertEquals(parseConnect("CONNECT evil.com/path:443 HTTP/1.1"), undefined);
});

/** Connects to the broker as an HTTPS_PROXY client would, sends one CONNECT. */
async function tryConnect(port: number, authority: string): Promise<string> {
  const conn = await Deno.connect({ hostname: "127.0.0.1", port });
  await conn.write(new TextEncoder().encode(`CONNECT ${authority} HTTP/1.1\r\n\r\n`));
  const buf = new Uint8Array(128);
  const n = await conn.read(buf);
  const reply = new TextDecoder().decode(buf.subarray(0, n ?? 0));
  try {
    conn.close();
  } catch {
    // The broker may have closed it on refusal.
  }
  return reply.split("\r\n")[0] ?? "";
}

Deno.test("the broker refuses what is not allowed, by name, port, and address", async () => {
  // Loopback is allowlisted here on purpose: even so, it is refused, because
  // the allowlist decides which names, and the address filter decides that the
  // host's own network is never one of them.
  const broker = new Broker(["127.0.0.1"], quiet);
  const port = broker.listen();
  try {
    // Allowed by name, but an internal address, so refused all the same.
    assertEquals((await tryConnect(port, "127.0.0.1:443")).includes("403"), true);
    // Not on the allowlist.
    assertEquals((await tryConnect(port, "derp1.tailscale.com:443")).includes("403"), true);
    // A port the broker will not open, refused before the address is weighed.
    assertEquals((await tryConnect(port, "127.0.0.1:5432")).includes("403"), true);
  } finally {
    broker.close();
  }
});

Deno.test("the broker refuses a non-CONNECT opener", async () => {
  const broker = new Broker(["github.com"], quiet);
  const port = broker.listen();
  try {
    const reply = await tryConnect(port, "").catch(() => "");
    const conn = await Deno.connect({ hostname: "127.0.0.1", port });
    await conn.write(new TextEncoder().encode("GET / HTTP/1.1\r\n\r\n"));
    const buf = new Uint8Array(64);
    const n = await conn.read(buf);
    const line = new TextDecoder().decode(buf.subarray(0, n ?? 0)).split("\r\n")[0] ?? "";
    conn.close();
    assertEquals(line.includes("400"), true);
    void reply;
  } finally {
    broker.close();
  }
});

Deno.test("the head is read whole, leaving the tunnelled bytes untouched", async () => {
  // A real client sends headers after the CONNECT line, then the blank line,
  // then its TLS. The broker must consume up to and including the blank line
  // and no further, or the leftover header bytes would be piped to the
  // upstream ahead of the ClientHello and break the handshake.
  const wire = new TextEncoder().encode(
    "CONNECT open.example.com:443 HTTP/1.1\r\n" +
      "Host: open.example.com:443\r\n" +
      "Proxy-Connection: keep-alive\r\n\r\n" +
      "TLS-CLIENT-HELLO",
  );
  let pos = 0;
  const reader = {
    read(p: Uint8Array): Promise<number | null> {
      if (pos >= wire.length) return Promise.resolve(null);
      p[0] = wire[pos++] as number;
      return Promise.resolve(1);
    },
  };
  const head = await readRequestHead(reader);
  assertEquals(head?.split(/\r?\n/, 1)[0], "CONNECT open.example.com:443 HTTP/1.1");
  // Everything after the blank line is still there for the tunnel to carry.
  assertEquals(new TextDecoder().decode(wire.subarray(pos)), "TLS-CLIENT-HELLO");
});

Deno.test("the credential is put on at the broker, never given to the session", async () => {
  // A stand-in provider that reports what key it was actually handed.
  let seen = "";
  const upstream = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen: () => {} }, (request) => {
    seen = request.headers.get("authorization") ?? "";
    return new Response("{}", { headers: { "content-type": "application/json" } });
  });
  const upstreamPort = (upstream.addr as Deno.NetAddr).port;

  const broker = new Broker([], quiet, [{
    prefix: "/provider",
    upstream: `http://127.0.0.1:${upstreamPort}/v4`,
    nonce: "the-session-nonce",
    credential: "the-real-key",
  }], true);
  const port = broker.listen();
  try {
    const allowed = await fetch(`http://127.0.0.1:${port}/provider/chat/completions`, {
      method: "POST",
      headers: { authorization: "Bearer the-session-nonce" },
      body: "{}",
    });
    await allowed.body?.cancel();
    assertEquals(allowed.status, 200);
    // The session never held this, and the provider still received it.
    assertEquals(seen, "Bearer the-real-key");

    // What a session could read out of its own environment is the nonce, and
    // a nonce the broker does not know is worth nothing.
    const refused = await fetch(`http://127.0.0.1:${port}/provider/chat/completions`, {
      method: "POST",
      headers: { authorization: "Bearer not-the-nonce" },
      body: "{}",
    });
    await refused.body?.cancel();
    assertEquals(refused.status, 401);
  } finally {
    broker.close();
    await upstream.shutdown();
  }
});

/**
 * A nonce is per provider, so reading one out of a session buys nothing
 * against another provider the same broker serves.
 */
Deno.test("each provider has its own route, and its own nonce", async () => {
  const seen: Record<string, string> = {};
  const one = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen: () => {} }, (r) => {
    seen.one = r.headers.get("authorization") ?? "";
    return new Response("{}");
  });
  const two = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen: () => {} }, (r) => {
    seen.two = r.headers.get("authorization") ?? "";
    return new Response("{}");
  });
  const onePort = (one.addr as Deno.NetAddr).port;
  const twoPort = (two.addr as Deno.NetAddr).port;

  const broker = new Broker([], quiet, [
    {
      prefix: "/provider/zai",
      upstream: `http://127.0.0.1:${onePort}/v4`,
      nonce: "nonce-zai",
      credential: "key-zai",
    },
    {
      prefix: "/provider/meta",
      upstream: `http://127.0.0.1:${twoPort}/v1`,
      nonce: "nonce-meta",
      credential: "key-meta",
    },
  ], true);
  const port = broker.listen();
  try {
    const a = await fetch(`http://127.0.0.1:${port}/provider/zai/chat`, {
      method: "POST",
      headers: { authorization: "Bearer nonce-zai" },
      body: "{}",
    });
    await a.body?.cancel();
    const b = await fetch(`http://127.0.0.1:${port}/provider/meta/chat`, {
      method: "POST",
      headers: { authorization: "Bearer nonce-meta" },
      body: "{}",
    });
    await b.body?.cancel();

    // Each upstream got its own key, and neither got the other's.
    assertEquals(seen.one, "Bearer key-zai");
    assertEquals(seen.two, "Bearer key-meta");

    // One provider's nonce is refused on another's route.
    const crossed = await fetch(`http://127.0.0.1:${port}/provider/meta/chat`, {
      method: "POST",
      headers: { authorization: "Bearer nonce-zai" },
      body: "{}",
    });
    await crossed.body?.cancel();
    assertEquals(crossed.status, 401);
  } finally {
    broker.close();
    await one.shutdown();
    await two.shutdown();
  }
});

Deno.test("host-internal addresses are refused, whatever the allowlist says", () => {
  for (const address of ["127.0.0.1", "10.0.0.1", "192.168.0.1", "169.254.169.254", "100.64.0.1"]) {
    assertEquals(isPrivateAddress(address), true);
  }
  for (const address of ["8.8.8.8", "1.1.1.1", "203.0.113.9", "2606:4700:4700::1111"]) {
    assertEquals(isPrivateAddress(address), false);
  }
  // Loopback and link-local in v6, and a v4 loopback wearing a v6 coat.
  for (const address of ["::1", "fe80::1", "fd00::1", "::ffff:127.0.0.1"]) {
    assertEquals(isPrivateAddress(address), true);
  }
});

/** A name on the allowlist that points at the host is still refused. */
Deno.test("a name is judged by where it resolves, not by its spelling", async () => {
  // Resolves to loopback: nothing to dial.
  assertEquals(
    await publicAddress("rebind.test", false, () => Promise.resolve(["127.0.0.1"])),
    undefined,
  );
  // Mixed: the public one is what gets dialled, and it is an address, so what
  // was judged is what is used rather than the name resolved a second time.
  assertEquals(
    await publicAddress("mixed.test", false, () => Promise.resolve(["10.0.0.1", "9.9.9.9"])),
    "9.9.9.9",
  );
  // A literal internal target does not even reach the resolver.
  assertEquals(
    await publicAddress("169.254.169.254", false, () => Promise.reject(new Error("x"))),
    undefined,
  );
});

/** The broker refuses to tunnel to the host's own loopback. */
Deno.test("a tunnel to loopback is refused even under a lone *", async () => {
  const broker = new Broker(["*"], quiet);
  const port = broker.listen();
  try {
    const reply = await tryConnect(port, "127.0.0.1:443");
    assertEquals(reply.includes("403"), true);
  } finally {
    broker.close();
  }
});

/**
 * An operator who names an internal address outright has said which machine
 * they mean. A name pointing there has not, because what it points at is not
 * theirs to decide, so it stays refused even with the flag on.
 */
Deno.test("allowInternal admits a literal address, never a name", async () => {
  assertEquals(await publicAddress("10.0.0.5", true), "10.0.0.5");
  assertEquals(await publicAddress("127.0.0.1", true), "127.0.0.1");
  assertEquals(await publicAddress("10.0.0.5", false), undefined);

  // A name that resolves internally is refused whether the flag is on or off.
  const resolver = () => Promise.resolve(["10.0.0.5"]);
  assertEquals(await publicAddress("mirror.internal", true, resolver), undefined);
  assertEquals(await publicAddress("mirror.internal", false, resolver), undefined);
});

Deno.test("an allowlisted internal address is dialled only when allowed on purpose", async () => {
  const off = new Broker(["127.0.0.1"], quiet, [], false);
  const offPort = off.listen();
  try {
    assertEquals((await tryConnect(offPort, "127.0.0.1:443")).includes("403"), true);
  } finally {
    off.close();
  }

  const on = new Broker(["127.0.0.1"], quiet, [], true);
  const onPort = on.listen();
  try {
    // Admitted now, so it gets as far as dialling: nothing listens on 443, so
    // it fails upstream rather than being refused at the gate.
    assertEquals((await tryConnect(onPort, "127.0.0.1:443")).includes("403"), false);
  } finally {
    on.close();
  }
});

/** A provider pointed at this machine is a setting, not a quiet exception. */
Deno.test("a provider at an internal address is refused unless allowed on purpose", async () => {
  const upstream = Deno.serve(
    { hostname: "127.0.0.1", port: 0, onListen: () => {} },
    () => new Response("{}"),
  );
  const upstreamPort = (upstream.addr as Deno.NetAddr).port;
  const route = {
    prefix: "/provider",
    upstream: `http://127.0.0.1:${upstreamPort}/v1`,
    nonce: "n",
    credential: "k",
  };

  const broker = new Broker([], quiet, [route], false);
  const port = broker.listen();
  try {
    const refused = await fetch(`http://127.0.0.1:${port}/provider/chat`, {
      method: "POST",
      headers: { authorization: "Bearer n" },
      body: "{}",
    });
    await refused.body?.cancel();
    assertEquals(refused.status, 502);
  } finally {
    broker.close();
    await upstream.shutdown();
  }
});

/**
 * The same address has many spellings. Matching the text let the spelling
 * decide: `::ffff:7f00:1` is loopback written in hex, and it was dialled as
 * 127.0.0.1 while reading as public.
 */
Deno.test("a v4 address in a v6 coat is judged as the v4 it carries", () => {
  for (
    const address of [
      "::ffff:7f00:1", // 127.0.0.1 in hex
      "::ffff:127.0.0.1", // the same, dotted
      "::FFFF:7F00:1", // the same, upper case
      "0:0:0:0:0:ffff:7f00:1", // the same, unabbreviated
      "::ffff:a9fe:a9fe", // 169.254.169.254, the metadata address
      "::ffff:0a00:1", // 10.0.0.1
      "::ffff:c0a8:1", // 192.168.0.1
      "::ffff:ac10:1", // 172.16.0.1
      "::7f00:1", // v4-compatible loopback
      "64:ff9b::7f00:1", // NAT64 of loopback
    ]
  ) {
    assertEquals(isPrivateAddress(address), true, `${address} must be refused`);
  }

  // A public v4 in a v6 coat is still public, in either spelling.
  assertEquals(isPrivateAddress("::ffff:0808:0808"), false);
  assertEquals(isPrivateAddress("::ffff:8.8.8.8"), false);
});

Deno.test("v6 forms that are internal in their own right", () => {
  for (const address of ["::1", "::", "fe80::1", "febf::1", "fc00::1", "fd12:3456::1", "ff02::1"]) {
    assertEquals(isPrivateAddress(address), true, `${address} must be refused`);
  }
  for (const address of ["2606:4700:4700::1111", "2001:4860:4860::8888"]) {
    assertEquals(isPrivateAddress(address), false, `${address} must be allowed`);
  }
  // Something that is not an address at all is not one to dial.
  assertEquals(isPrivateAddress("::ffff:zzzz:1"), true);
  assertEquals(isPrivateAddress("1:2:3:4:5:6:7:8:9"), true);
});
