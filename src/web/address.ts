/**
 * Which addresses the interface may bind to.
 *
 * The interface has no login. Reaching it is the authorisation, so the address
 * it listens on is the entire access control and is checked rather than
 * trusted. A public bind is refused at startup instead of warned about, because
 * a warning in a log is not a control.
 *
 * The tailnet range is allowed deliberately: reaching a machine over a private
 * overlay network is the intended way to use this from a phone.
 */

/** Why an address was refused, phrased for someone reading a startup failure. */
export interface AddressVerdict {
  allowed: boolean;
  reason: string;
}

const LOOPBACK_NAMES = new Set(["localhost", "127.0.0.1", "::1", "[::1]"]);

/** Any address on this machine, which is what makes a bind public. */
const WILDCARDS = new Set(["0.0.0.0", "::", "[::]", "*"]);

function parseIPv4(host: string): number[] | null {
  const parts = host.split(".");
  if (parts.length !== 4) return null;

  const octets: number[] = [];
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) return null;
    const value = Number.parseInt(part, 10);
    if (value > 255) return null;
    octets.push(value);
  }
  return octets;
}

function isPrivateIPv4(octets: number[]): boolean {
  const [a = 0, b = 0] = octets;
  if (a === 127) return true; // loopback
  if (a === 10) return true; // RFC 1918
  if (a === 192 && b === 168) return true; // RFC 1918
  if (a === 172 && b >= 16 && b <= 31) return true; // RFC 1918
  if (a === 169 && b === 254) return true; // link local
  // RFC 6598 shared address space, which is where a tailnet lives.
  if (a === 100 && b >= 64 && b <= 127) return true;
  return false;
}

function isPrivateIPv6(host: string): boolean {
  const bare = host.replace(/^\[|\]$/g, "").toLowerCase();
  if (bare === "::1") return true;
  if (bare.startsWith("fe80:")) return true; // link local
  // Unique local addresses, fc00::/7.
  return /^f[cd]/.test(bare);
}

/**
 * Decides whether the interface may listen on an address.
 *
 * @returns why not, when it may not, so the refusal can say something useful.
 */
export function checkBindAddress(host: string): AddressVerdict {
  const trimmed = host.trim().toLowerCase();

  if (trimmed.length === 0) {
    return { allowed: false, reason: "no address was given" };
  }

  if (WILDCARDS.has(trimmed)) {
    return {
      allowed: false,
      reason:
        `${host} listens on every interface, including public ones. Bind to a loopback or private address instead.`,
    };
  }

  if (LOOPBACK_NAMES.has(trimmed)) return { allowed: true, reason: "loopback" };

  const octets = parseIPv4(trimmed);
  if (octets !== null) {
    return isPrivateIPv4(octets) ? { allowed: true, reason: "private address" } : {
      allowed: false,
      reason:
        `${host} is a public address, and the interface has no login. Bind to a loopback, private, or tailnet address instead.`,
    };
  }

  if (trimmed.includes(":")) {
    return isPrivateIPv6(trimmed) ? { allowed: true, reason: "private address" } : {
      allowed: false,
      reason:
        `${host} is a public address, and the interface has no login. Bind to a loopback, private, or tailnet address instead.`,
    };
  }

  // A hostname could resolve anywhere, and resolving it here would mean the
  // check depended on DNS at the moment of startup.
  return {
    allowed: false,
    reason:
      `${host} is a name, not an address. Give the address to bind to, so what is reachable does not depend on what a name resolves to.`,
  };
}
