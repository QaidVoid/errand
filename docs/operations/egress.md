# Network egress

By default every connection a session makes goes through an **egress broker**
of its own. The broker decides which hosts the session may reach, holds the real
provider keys so the sandbox never does, and records every provider call.

## Modes

| `sandbox.egress.mode` | What a session can reach |
| --- | --- |
| `proxy` (default) | Only its broker. The broker forwards to allowed hosts on allowed ports. |
| `open` | No broker. Under bailey, any host on the allowed ports. Under podman, anything its network namespace allows. Under Incus, the profile NIC directly. |

`sandbox.network: "none"` takes the network away entirely, whatever the mode.

## Choosing hosts

With no `egress` block at all, the allowlist is `["*"]`. The broker still
enforces ports, refuses the host's own network, and logs every connection, but
it does not restrict hosts. To hold sessions to the hosts your projects need,
name them:

```json
{
  "sandbox": {
    "egress": { "allow": ["api.anthropic.com", "github.com", "*.github.com"] }
  }
}
```

- An `egress` block without `allow` allows no hosts. Only a missing block means
  `["*"]`.
- Brokered providers are reached through the broker's own provider route, which
  this list does not govern. The host the model store names for the configured
  model is added automatically.
- A provider that is not brokered, marked `broker: false` or registered by a
  plugin, is reached like any other host, so list its host.
- With an empty list and no provider host, the daemon refuses to start.

### Ports

```json
{ "sandbox": { "egressPorts": [80, 443] } }
```

The default is `[443]`, which is all a provider needs. Add `80` for a mirror or
redirect that still uses plain HTTP. With the broker, these are the only ports
it opens upstream. Without it, bailey enforces the list in its policy, and
podman ignores it.

## Granting a host from the thread

When a session tries a host that is not allowed, the broker refuses it at once
with `403`. It never holds the connection while somebody decides. The thread is
told what to ask for, at most once a minute per host and for three hosts a
minute:

```
this session tried to reach `api.example.com`, which is not on the egress
allowlist or granted. `!grant api.example.com` lets its next connection
through, and `!grant api.example.com 10m` or `!grant api.example.com always`
lets it through for longer
```

| Command | Grants |
| --- | --- |
| `!grant <host>` | The next connection to the host, and no more. |
| `!grant <host> 10m` | Connections for that long, written as `30s`, `2h`, `7d 12h` and so on. |
| `!grant <host> always` | The rest of the session, including after it resumes. |
| `!grant` | Nothing. Lists current grants, time left, and who granted them. |
| `!revoke <host>` | Drops every grant for the host, and warns when a wildcard still covers it. |

- A host may be written as `*.example.com`. A grant never covers a path, opens
  a port outside `egressPorts`, or reaches an address the broker refuses anyway.
- Only the owner and operators may grant.
- Grants are per session and kept in its record directory, where the session
  cannot write. They hold hosts, times, and account ids, never credentials.
- Revoking a host does not cut a connection already open.

With the default `["*"]`, nothing is ever refused, so grants matter only once
you narrow the list.

## How the broker works

Each session's broker starts when its sandbox launches, on a loopback port no
other session uses, and stops before the session does.

**Credentials.** The broker mints a proxy credential and a stand-in for every
brokered provider key, each `errand-session-` followed by 64 hex digits. They
are valid only at that session's broker and only until the session stops. The
session's proxy variables carry the proxy credential as URL userinfo, so
ordinary tools send it automatically. A request without it gets
`407 Proxy Authentication Required` before anything is dialled.

**Resolving names.** The broker resolves names itself with the host's resolver
and dials only addresses it checked. A name that resolves only to the host's
own network (loopback, private, link-local) or to a cloud fabric address such
as Azure's `168.63.129.16` is refused with `403 not a public host`.
`sandbox.egress.allowInternal` lifts that only for an address written as a
literal IP. A name that does not resolve gets
`502 the name did not resolve`, and the log has the resolver's reason.

**Timeouts.** A lookup gets 5 seconds and the dial 10 seconds across all of a
name's addresses, answered with `504` or `502` before a client's own timeout,
and the log names the host and the phase that failed.

**Reloads.** A configuration reload rebuilds the broker settings for sessions
launched afterwards. Running sessions keep the routes they started with.

### Under each backend

- **bailey** runs the session in a private network namespace whose only exit is
  the broker, with UDP, ICMP, and DNS dropped.
- **podman** gives a brokered container loopback only. pasta forwards its
  loopback port to the broker, and nothing else resolves or connects.
- **Incus** removes the NIC and reaches the broker through a read-only Unix
  socket mount and a relay on the guest's loopback. See
  [the Incus backend](/operations/incus#network).

### Hiding the host address

Under bailey with `egress.mode = open`, a session shares the host's network
namespace and can read its address, MAC, and ARP neighbours. Set
`sandbox.hideHostAddress` to give it a private namespace with a synthetic
address instead. This needs `pasta` on the host. Without it the session still
starts, and says the host address is visible. Podman and brokered sessions
already have their own network.

## Provider routes

The broker calls brokered providers itself, with the real key, and applies the
same address rules as it dials. It always connects directly, ignoring any proxy
variables in the daemon's own environment.

- The provider's base URL must be `https://`. Plain `http://` is allowed only to
  a literal loopback address, such as `http://127.0.0.1:8080`, and only with
  `sandbox.egress.allowInternal`.
- The broker follows no redirects, since a redirect would carry the key to a
  host nobody checked. A provider that answers with a redirect does not work
  behind the broker.

### Which header carries the key

`placement` says where the key goes: `"bearer"` for `Authorization: Bearer`, or
`"x-api-key"`. The broker accepts the stand-in only in that header, answers
`401` naming the right header when it arrives elsewhere, and sends the real key
only there.

Without `placement`, an explicit `kind` decides: `anthropic` means `x-api-key`,
and `openai` means `bearer`. With neither, as for providers kage already knows,
the stand-in is accepted in either header. The image, speech, and music tools
send a bearer token, so a provider carrying one of them cannot use
`x-api-key`.

### Which calls the key may make

`capabilities` lists the method and path pairs the key may be used for, or
`"*"` for every call:

```json
{
  "agent": {
    "providers": {
      "anthropic": {
        "credential": "...",
        "placement": "x-api-key",
        "capabilities": [
          { "method": "POST", "path": "/v1/messages" },
          { "method": "GET", "path": "/v1/models" }
        ],
        "strictCapabilities": true
      }
    }
  }
}
```

- A path is matched against what follows the provider's route, so it reads as
  the provider's own API. It is matched as sent, before decoding, and never
  against the query.
- `*` matches one path segment and `**` any number, including none. Methods are
  upper case, or `*`.
- Without `strictCapabilities`, an unlisted call is forwarded and logged. With
  it, the call is answered `403` and never reaches the provider.
- A provider without `capabilities` forwards everything and logs each call at
  `info` with the entry that would allow it, so you can write the list from a
  day's log.
- The image, speech, and music tools use the same route, so a strict list must
  name their endpoints too, such as `POST /images/generations`.

An empty list is refused, and so is `strictCapabilities` without a list. A key
that looks like a misspelling of `placement`, `capabilities` or
`strictCapabilities` is refused as well, because a misspelt one would leave the
route open.

Whatever the list says, a path the provider could read differently from how it
was matched is refused with `400`: empty segments, `.` or `..` segments, a
segment that decodes to one of those or to a slash, or `;` parameters. Method
and URL override headers such as `X-HTTP-Method-Override` are not forwarded. A
`_method` field in a body or query is not inspected.

`placement` and `capabilities` are read when a session launches. A tunnel is
opaque once open, so tunnels are governed by host alone, and only provider
routes can be held to a method and path.

### Scrubbing provider answers

An upstream that quotes the request back, as a verbose error page might, would
hand the session the real key. The broker replaces every occurrence of that
provider's key with `[redacted]` in the streamed body and in every header.

To read the body, the broker asks for an uncompressed answer. One that arrives
compressed anyway is refused with `502` and the warning
`a provider answered compressed though asked not to`, so a provider that always
compresses cannot be used behind the broker. An answer over 64 MiB is refused
when it says so up front, and cut when it streams past that.

This does not catch a key that the provider re-encodes (JSON escapes, URL
encoding, base64) or quotes only in part.

## Receipts

Every provider call a broker handles is recorded in `receipts.db` under the
state directory. A receipt holds:

- when the call ended, the session, the provider, and its credential's variable
  name
- the host, method, path, status, duration, and bytes each way
- the outcome: `completed`, `interrupted`, `denied`, `refused`, or
  `upstream-unreachable`

No header, body, credential, or stand-in is ever stored. The path is decoded,
scrubbed of stand-ins, cut at any query (kept only as a length, since it can
carry a key), and cut to 256 bytes. `denied` calls are recorded at most once a
minute, with the log counting the rest.

Tunnels carry no brokered credential and leave no receipt, so their log lines
are their only record. `errand threads show <thread>` lists the newest calls a
session made, and receipts are deleted with the session. A receipt that cannot
be written does not fail the call, and the log says so.
