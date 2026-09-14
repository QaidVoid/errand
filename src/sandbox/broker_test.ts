import { assertEquals } from "@std/assert";
import { Broker, hostAllowed, parseConnect } from "./broker.ts";

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

Deno.test("the broker tunnels an allowlisted host and refuses everything else", async () => {
  // A stand-in "upstream" the broker is allowed to reach: a loopback TCP
  // server. The allowlist names 127.0.0.1, so a CONNECT to it is admitted and
  // anything else is refused with 403, the way a tailnet relay would be.
  const upstream = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const upstreamPort = (upstream.addr as Deno.NetAddr).port;
  (async () => {
    for await (const c of upstream) {
      c.close();
    }
  })();

  const broker = new Broker(["127.0.0.1"], quiet);
  const port = broker.listen();
  try {
    const allowed = await tryConnect(port, `127.0.0.1:443`);
    // 443 is the only upstream port the broker opens, so the host is admitted
    // but the connect to a closed 443 fails upstream: either way it is not 403.
    assertEquals(allowed.includes("403"), false);

    const denied = await tryConnect(port, `derp1.tailscale.com:443`);
    assertEquals(denied.includes("403"), true);

    // An allowlisted host on a port the broker will not open is still refused.
    const oddPort = await tryConnect(port, `127.0.0.1:${upstreamPort}`);
    assertEquals(oddPort.includes("403"), true);
  } finally {
    broker.close();
    upstream.close();
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
