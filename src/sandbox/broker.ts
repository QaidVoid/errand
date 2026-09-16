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
 * Where the model provider really is, and what stands in for its key.
 *
 * A session is given `nonce` in place of the credential, so the credential
 * itself never enters a sandbox and nothing read out of a session's
 * environment can be replayed anywhere else. The nonce is worth only what the
 * broker will do with it, and the broker is reachable only from the session's
 * own namespace.
 */
export interface ProviderRoute {
  /** Path a session addresses the provider at, such as `/provider`. */
  prefix: string;
  /** The provider's real base URL, which the prefix stands in for. */
  upstream: string;
  /** What a session sends as its key. */
  nonce: string;
  /** The real credential. Never leaves the daemon. */
  credential: string;
}

/** Headers that describe one hop and must not be forwarded to the next. */
const HOP_BY_HOP = [
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "host",
];

/** Compares without letting the time taken say how much of it matched. */
function sameSecret(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let differences = 0;
  for (let index = 0; index < a.length; index += 1) {
    differences |= a.charCodeAt(index) ^ b.charCodeAt(index);
  }
  return differences === 0;
}

/**
 * Whether an IPv4 address is one the broker must not connect to.
 *
 * The broker runs on the host, so a connection it makes reaches the host's own
 * network from the host's position. Loopback, the private ranges, link-local,
 * and the carrier range are the host and its neighbours, not the internet a
 * session is allowed out to. Reaching them through the broker would be a way
 * back into the host that the network namespace was built to close.
 */
export function isPrivateV4(address: string): boolean {
  const parts = address.split(".").map((part) => Number(part));
  if (parts.length !== 4 || parts.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) {
    // Not a dotted quad. Treated as private, because an address the broker
    // cannot read is not one it should dial.
    return true;
  }
  const [a, b] = parts as [number, number, number, number];
  if (a === 0 || a === 127 || a === 10 || a === 255) return true; // this host, loopback, private, broadcast
  if (a === 169 && b === 254) return true; // link-local, which is the metadata address
  if (a === 172 && b >= 16 && b <= 31) return true; // private
  if (a === 192 && b === 168) return true; // private
  if (a === 100 && b >= 64 && b <= 127) return true; // carrier-grade NAT
  if (a >= 224) return true; // multicast and reserved
  return false;
}

/** Whether an IPv6 address is one the broker must not connect to. */
export function isPrivateV6(address: string): boolean {
  const lower = address.toLowerCase().split("%")[0] as string;
  if (lower === "::1" || lower === "::") return true; // loopback, unspecified
  if (
    lower.startsWith("fe80") || lower.startsWith("fe9") || lower.startsWith("fea") ||
    lower.startsWith("feb")
  ) {
    return true; // link-local
  }
  if (lower.startsWith("fc") || lower.startsWith("fd")) return true; // unique local
  // A v4 address wearing a v6 coat reaches the same v4 host, so it is judged
  // as the v4 it carries.
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(lower);
  if (mapped) return isPrivateV4(mapped[1] as string);
  return false;
}

/** Whether a literal IP address is a host-internal one. */
export function isPrivateAddress(address: string): boolean {
  return address.includes(":") ? isPrivateV6(address) : isPrivateV4(address);
}

/**
 * Resolves a target to an address the broker may dial, or nothing.
 *
 * A name is resolved here and the result is judged, so a name on the allowlist
 * that points at the host's own network, whether by mistake or to slip past the
 * allowlist, is refused. The connection is then made to the address that was
 * judged rather than to the name resolved a second time, so what was checked is
 * what is dialled.
 *
 * `allowInternal` relaxes this for a literal address only. An operator naming
 * `10.0.0.5` has said which machine they mean, and no one else can change what
 * that points at. A name resolving somewhere internal stays refused even then,
 * because what a name points at is not the operator's to decide.
 */
export async function publicAddress(
  host: string,
  allowInternal = false,
  resolve: (name: string) => Promise<string[]> = defaultResolve,
): Promise<string | undefined> {
  const literal = /^\d+\.\d+\.\d+\.\d+$/.test(host) || host.includes(":");
  if (literal) {
    if (!isPrivateAddress(host)) return host;
    return allowInternal ? host : undefined;
  }
  const addresses = await resolve(host).catch(() => []);
  return addresses.find((address) => !isPrivateAddress(address));
}

async function defaultResolve(name: string): Promise<string[]> {
  const found: string[] = [];
  for (const kind of ["A", "AAAA"] as const) {
    try {
      found.push(...await Deno.resolveDns(name, kind));
    } catch {
      // No records of this kind; the other kind may still answer.
    }
  }
  return found;
}

/**
 * A running CONNECT proxy that admits only allowlisted hosts.
 *
 * Injected connect/accept functions keep it testable without real sockets in
 * the unit path, while the default uses Deno's TCP. One instance serves one
 * session's namespace; the allowlist is fixed for the life of that session.
 *
 * When a {@link ProviderRoute} is given it also answers as the provider
 * itself, on the same port: a request that is not a CONNECT is served rather
 * than refused, and the credential is put on at this end.
 */
export class Broker {
  private listener: Deno.Listener | null = null;
  private closed = false;
  private provider: Deno.HttpServer | null = null;
  private providerPort = 0;

  constructor(
    private readonly allow: readonly string[],
    private readonly log: Pick<Logger, "info" | "warn">,
    private readonly routes: readonly ProviderRoute[] = [],
    private readonly allowInternal = false,
  ) {}

  /**
   * Serves the provider API, with the real credential put on here.
   *
   * Run on its own loopback port and reached by handing the connection over,
   * rather than by parsing HTTP on the raw socket: a request body may be
   * streamed and a response is often an event stream, and getting either wrong
   * would show up as a session that hangs rather than one that fails.
   */
  private startProvider(routes: readonly ProviderRoute[]): void {
    this.provider = Deno.serve({
      hostname: "127.0.0.1",
      port: 0,
      onListen: (addr) => {
        this.providerPort = addr.port;
      },
    }, async (request) => {
      const url = new URL(request.url);
      // Longest prefix first, so a provider named under another's path is
      // still reached rather than shadowed by it.
      const route = [...routes]
        .sort((a, b) => b.prefix.length - a.prefix.length)
        .find((candidate) => url.pathname.startsWith(candidate.prefix));
      if (route === undefined) {
        return new Response("not a provider this broker serves\n", { status: 404 });
      }
      const offered = request.headers.get("authorization") ?? "";
      if (!sameSecret(offered, `Bearer ${route.nonce}`)) {
        this.log.warn("a provider call arrived without this session's key");
        return new Response("not this session\n", { status: 401 });
      }

      const rest = url.pathname.slice(route.prefix.length);
      const target = `${route.upstream.replace(/\/$/, "")}${rest}${url.search}`;
      // The provider is the operator's to name, so this is not a session
      // reaching somewhere it chose. It is still judged by the same rule, so
      // that a provider pointed at this machine is a deliberate setting rather
      // than a quiet exception to where the broker will go.
      if (await publicAddress(new URL(target).hostname, this.allowInternal) === undefined) {
        this.log.warn("a provider is configured at a host-internal address", {
          provider: route.prefix,
        });
        return new Response("the provider is not at a reachable address\n", { status: 502 });
      }
      const headers = new Headers();
      for (const [name, value] of request.headers) {
        if (!HOP_BY_HOP.includes(name.toLowerCase())) headers.set(name, value);
      }
      headers.set("authorization", `Bearer ${route.credential}`);

      try {
        const answered = await fetch(target, {
          method: request.method,
          headers,
          ...(request.body === null ? {} : { body: request.body }),
          redirect: "manual",
        });
        const back = new Headers();
        for (const [name, value] of answered.headers) {
          if (!HOP_BY_HOP.includes(name.toLowerCase())) back.set(name, value);
        }
        return new Response(answered.body, { status: answered.status, headers: back });
      } catch (error) {
        this.log.warn("the provider could not be reached", { detail: String(error) });
        return new Response("the provider could not be reached\n", { status: 502 });
      }
    });
  }

  /** Binds to a loopback port and starts admitting connections. Returns the port. */
  listen(host = "127.0.0.1"): number {
    if (this.routes.length > 0) this.startProvider(this.routes);
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
    const head = await readRequestHead(client);
    const requestLine = head?.split(/\r?\n/, 1)[0];
    const target = requestLine === undefined ? undefined : parseConnect(requestLine);
    if (target === undefined) {
      // Not a tunnel. With a provider route this is the session calling the
      // provider, which is served rather than refused: the head already read
      // is replayed so the server sees the request whole.
      if (head !== undefined && this.routes.length > 0) {
        await this.serveProvider(client, head);
        return;
      }
      await refuse(client, 400, "the broker speaks only CONNECT");
      return;
    }
    if (!ALLOWED_UPSTREAM_PORTS.includes(target.port) || !hostAllowed(target.host, this.allow)) {
      this.log.info("egress refused", { host: target.host, port: target.port });
      await refuse(client, 403, "not on the egress allowlist");
      return;
    }

    // Where the name actually points is checked, not just whether it is
    // allowed: the broker runs on the host, so dialling the host's own network
    // through it is a way back in that the namespace was built to close.
    const address = await publicAddress(target.host, this.allowInternal);
    if (address === undefined) {
      this.log.warn("egress refused a host-internal target", { host: target.host });
      await refuse(client, 403, "not a public host");
      return;
    }

    let upstream: Deno.Conn;
    try {
      upstream = await Deno.connect({ hostname: address, port: target.port });
    } catch (error) {
      await refuse(client, 502, "upstream unreachable");
      this.log.warn("upstream connect failed", { host: target.host, detail: String(error) });
      return;
    }
    await client.write(new TextEncoder().encode("HTTP/1.1 200 Connection Established\r\n\r\n"));
    this.log.info("egress allowed", { host: target.host, port: target.port });
    await pipe(client, upstream);
  }

  /**
   * Hands a connection to the provider server, head and all.
   *
   * The head was read to find out whether this was a tunnel, so it is written
   * on before the two are joined; everything after it is still in the socket
   * and flows through untouched, body and event stream alike.
   */
  private async serveProvider(client: Deno.Conn, head: string): Promise<void> {
    let inner: Deno.Conn;
    try {
      inner = await Deno.connect({ hostname: "127.0.0.1", port: this.providerPort });
    } catch (error) {
      this.log.warn("the provider endpoint is not up", { detail: String(error) });
      await refuse(client, 502, "the provider endpoint is not up");
      return;
    }
    await inner.write(new TextEncoder().encode(head));
    await pipe(client, inner);
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
    void this.provider?.shutdown().catch(() => {});
  }
}

/**
 * Reads the whole CONNECT request head, up to and including the blank line.
 *
 * Read one byte at a time so nothing past the head is consumed: what follows is
 * the tunnelled bytes, and reading even one of them here would strip it from
 * the stream the client expects to carry its TLS. Reading only the request
 * line, and leaving the remaining headers in the socket, is worse still: those
 * leftover header bytes would then be piped to the upstream ahead of the TLS
 * ClientHello and corrupt the connection. The returned text keeps CRs so the
 * caller splits on either line ending.
 */
export async function readRequestHead(
  conn: { read(p: Uint8Array): Promise<number | null> },
): Promise<string | undefined> {
  const buf = new Uint8Array(1);
  const bytes: number[] = [];
  // A head larger than this is not one the broker will honour.
  while (bytes.length < 8192) {
    const n = await conn.read(buf);
    if (n === null) return undefined;
    if (n === 0) continue;
    bytes.push(buf[0] as number);
    const len = bytes.length;
    // End of head: a blank line, as CRLFCRLF or a bare LFLF.
    if (bytes[len - 1] === 0x0a) {
      if (len >= 2 && bytes[len - 2] === 0x0a) break;
      if (
        len >= 4 && bytes[len - 2] === 0x0d && bytes[len - 3] === 0x0a && bytes[len - 4] === 0x0d
      ) {
        break;
      }
    }
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
