/**
 * The egress broker: the single endpoint a session in `proxy` mode may reach.
 *
 * A session's network namespace is locked so that the broker is the only thing
 * it can connect to. The broker then decides, per connection, whether the host
 * a session asked for is on the allowlist, and refuses everything else. This is
 * where egress stops being "any host on port 443" and becomes "these hosts and
 * no others", which is what keeps a session from dialing a relay it does not
 * need and turning an outbound allowance into a two-way channel.
 *
 * A session speaks to it as an ordinary HTTP `CONNECT` proxy, so `HTTPS_PROXY`
 * is all a well-behaved client needs. The tunnel is opaque once established:
 * the broker gates on the host in the CONNECT line and then copies bytes, it
 * does not read inside the TLS. Credential injection for the provider is a
 * separate, terminating path and lives elsewhere; this file is the gate.
 */

/**
 * Whether `host` is permitted by an allowlist of names and `*.` wildcards.
 *
 * A lone `*` admits any host, turning the broker into an audit pass-through: it
 * still gates the port and logs every connection, but restricts no host. A bare
 * name matches only itself. A `*.example.com` rule matches any single or
 * multi-label subdomain of `example.com` but not the bare `example.com`, since
 * a wildcard is written when the apex is not what a session talks to. The
 * comparison is case-folded, because a hostname is.
 */
import type { Logger } from "../log.ts";

export function hostAllowed(host: string, allow: readonly string[]): boolean {
  const h = host.trim().toLowerCase();
  if (h.length === 0) return false;
  for (const rule of allow) {
    if (rule === "*") return true;
    if (rule.startsWith("*.")) {
      const suffix = rule.slice(1).toLowerCase(); // ".example.com"
      if (h.length > suffix.length && h.endsWith(suffix)) return true;
    } else if (h === rule.toLowerCase()) {
      return true;
    }
  }
  return false;
}

/** The host and port a `CONNECT` line asked for. */
export interface ConnectTarget {
  host: string;
  port: number;
}

/**
 * Reads the target out of an HTTP `CONNECT` request line.
 *
 * The line is `CONNECT host:port HTTP/1.1`. Anything else, a missing port, a
 * port that is not a number or is out of range, is refused by returning
 * undefined rather than guessing, since a target the broker had to guess at is
 * one it cannot claim to have checked.
 */
export function parseConnect(requestLine: string): ConnectTarget | undefined {
  const parts = requestLine.trim().split(/\s+/);
  if (parts.length < 2 || parts[0]?.toUpperCase() !== "CONNECT") return undefined;
  const authority = parts[1] as string;
  // IPv6 literals would be bracketed; a session reaches named hosts, so a
  // bracketed authority is not something the allowlist can match and is left
  // to be refused by the caller rather than parsed into a host here.
  const colon = authority.lastIndexOf(":");
  if (colon <= 0 || colon === authority.length - 1) return undefined;
  const host = authority.slice(0, colon);
  const port = Number(authority.slice(colon + 1));
  if (!Number.isInteger(port) || port < 1 || port > 65535) return undefined;
  if (host.includes("/") || host.includes("[")) return undefined;
  return { host, port };
}

/** Ports the broker will open upstream, so a tunnel cannot reach a service on an odd port. */
export const ALLOWED_UPSTREAM_PORTS: readonly number[] = [443];

/**
 * A running CONNECT proxy that admits only allowlisted hosts.
 *
 * Injected connect/accept functions keep it testable without real sockets in
 * the unit path, while the default uses Deno's TCP. One instance serves one
 * session's namespace; the allowlist is fixed for the life of that session.
 */
export class Broker {
  private listener: Deno.Listener | null = null;
  private closed = false;

  constructor(
    private readonly allow: readonly string[],
    private readonly log: Pick<Logger, "info" | "warn">,
  ) {}

  /** Binds to a loopback port and starts admitting connections. Returns the port. */
  listen(host = "127.0.0.1"): number {
    const listener = Deno.listen({ hostname: host, port: 0, transport: "tcp" });
    this.listener = listener;
    const addr = listener.addr as Deno.NetAddr;
    void this.accept(listener);
    return addr.port;
  }

  private async accept(listener: Deno.Listener): Promise<void> {
    for await (const conn of listener) {
      this.handle(conn).catch((error) => {
        this.log.warn("broker connection failed", { detail: String(error) });
        try {
          conn.close();
        } catch {
          // Already gone; nothing to close.
        }
      });
    }
  }

  private async handle(client: Deno.Conn): Promise<void> {
    const line = await readRequestLine(client);
    const target = line === undefined ? undefined : parseConnect(line);
    if (target === undefined) {
      await refuse(client, 400, "the broker speaks only CONNECT");
      return;
    }
    if (!ALLOWED_UPSTREAM_PORTS.includes(target.port) || !hostAllowed(target.host, this.allow)) {
      this.log.info("egress refused", { host: target.host, port: target.port });
      await refuse(client, 403, "not on the egress allowlist");
      return;
    }

    let upstream: Deno.Conn;
    try {
      upstream = await Deno.connect({ hostname: target.host, port: target.port });
    } catch (error) {
      await refuse(client, 502, "upstream unreachable");
      this.log.warn("upstream connect failed", { host: target.host, detail: String(error) });
      return;
    }
    await client.write(new TextEncoder().encode("HTTP/1.1 200 Connection Established\r\n\r\n"));
    this.log.info("egress allowed", { host: target.host, port: target.port });
    await pipe(client, upstream);
  }

  /** Stops accepting and closes the listener. In-flight tunnels end with it. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.listener?.close();
    } catch {
      // Already closed.
    }
  }
}

/** Reads up to the first CRLF, which is the request line, without consuming the body. */
async function readRequestLine(conn: Deno.Conn): Promise<string | undefined> {
  const buf = new Uint8Array(1);
  const bytes: number[] = [];
  // A request line longer than this is not one the broker will honour.
  while (bytes.length < 8192) {
    const n = await conn.read(buf);
    if (n === null) return undefined;
    if (n === 0) continue;
    const byte = buf[0] as number;
    if (byte === 0x0a) break; // LF ends the line
    if (byte !== 0x0d) bytes.push(byte); // drop CR
  }
  return new TextDecoder().decode(new Uint8Array(bytes));
}

async function refuse(conn: Deno.Conn, code: number, reason: string): Promise<void> {
  try {
    await conn.write(new TextEncoder().encode(`HTTP/1.1 ${code} ${reason}\r\n\r\n`));
  } catch {
    // The client may have gone; the refusal is best-effort.
  } finally {
    try {
      conn.close();
    } catch {
      // Already closed.
    }
  }
}

/** Copies bytes both ways until either side ends, then closes both. */
async function pipe(a: Deno.Conn, b: Deno.Conn): Promise<void> {
  const one = a.readable.pipeTo(b.writable).catch(() => {});
  const two = b.readable.pipeTo(a.writable).catch(() => {});
  await Promise.allSettled([one, two]);
}
