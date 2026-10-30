# Sandboxing

Every session runs confined. There is no unsandboxed mode and no flag that
turns it off, because the agent is the untrusted party: it runs commands written
by a model against instructions written by whoever can post in a channel.

## What confines it

**bailey** confines a session as a host process, using Landlock for the
filesystem, seccomp for the syscall surface, and user, PID, and UTS namespaces.
Sessions use the host's own tools, so there is no image to build or keep.

**podman**, rootless, runs the session in a container from an image you provide.
Use it when you would rather the session saw a filesystem you assembled than the
host's.

Either way, the daemon asks the kage that sessions run which providers it
knows when it starts: the host's under bailey, and the image's under podman. A
provider kage already knows is written into its configuration as an override,
keeping kage's models, and any other as a custom provider. A configuration
naming one kage would refuse, such as `acp`, which it reserves, is refused
when the daemon starts and on every reload. Without that check, every session
would fail at launch. A kage too old to answer stops the daemon starting.

Both hold a session to one project directory and one state directory. What a
session may reach on the network depends on the backend and on
`sandbox.egress.mode`, and out of the box it is more than the model provider.

With `sandbox.egress.mode` set to `proxy`, the default, every connection a
session makes goes through its egress broker, under either backend. With no
`egress` block written, the built-in allowlist is `["*"]`. The broker then still
gates the port, refuses the host's own network and Azure's fabric address, and
logs every connection, but restricts no host. To hold sessions to the hosts a
project needs, name them, and no other host is reached through the broker:

```json
{
  "sandbox": {
    "egress": { "allow": ["api.anthropic.com", "github.com", "*.github.com"] }
  }
}
```

An `egress` block that leaves out `allow` names no host. Only a block left out
altogether means `["*"]`. A brokered provider is reached through the broker's
own provider route, which this list does not govern, and the host the model
store names for the configured model is added to the list. A provider that is
not brokered, one marked `broker: false` or registered by an `extension`, is
reached as any other host, so name its host when the list is not `["*"]`.
With an empty list and no provider host from the store, the daemon refuses to
start.

With `sandbox.egress.mode` set to `open`, there is no broker. Under bailey
the generated policy admits any host on the allowed ports, the host's own
network included. Under podman the container's network is bounded by its
namespace alone, so a session reaches any host on any port. Either way
`sandbox.egress.allow` does nothing.

Under podman a brokered container is given loopback and nothing else. pasta
forwards its loopback port to its broker's port on the host, and every other
address, public or private and the host's own included, is unreachable, so
nothing resolves either: the broker resolves names itself.

If a session needs another outbound port, name it under
`sandbox.egressPorts`:

```json
{
  "sandbox": {
    "network": "restricted",
    "egressPorts": [80, 443]
  }
}
```

The default is `[443]`, which is all a provider needs. Adding 80 lets a session
speak plaintext HTTP, for a mirror or a redirect that has not moved to TLS. With
`sandbox.egress.mode` set to `proxy`, the broker opens only these ports
upstream, for `CONNECT` tunnels and plain `http://` requests alike, and holds
both to the same allowlist, under either backend. Without the broker, the
bailey backend enforces the list in the generated policy, while podman bounds
the network by namespace rather than by port, so the list is inert there. A
session with `network` set to `none` opens nothing, whatever ports are named.

Each session has a broker of its own. It is started when the session's
sandbox launches, on a loopback port no other session uses, and it mints a
proxy credential and a stand-in for every brokered provider key for that
session alone. Each reads `errand-session-` and 64 hex digits, so an echoed
one is plainly errand's rather than a provider key. A credential or stand-in
read out of one session is refused by every other session's broker. Stopping
the session closes its broker first: from then on neither is honoured, even
on a connection the session opened earlier, and the port is released. A
tunnel already open is not cut, and ends when the session's processes do. The
broker's log lines name the session they belong to, tunnels included. A
reload of the configuration rebuilds what brokers are made from, so a session
launched after it routes the providers the file then defines, and a session
already running keeps the routes it was launched with.

Every provider call a session's broker handles is recorded as well, in
`receipts.db` under the state directory. A receipt holds when the call ended,
the session, the provider, the name of the variable its credential is configured
under, the host the call went to, the method and path, the status, how long it
took, how many bytes went each way, and what came of it: `completed`,
`interrupted` when the answer failed, was cut for its size, or the session hung
up part way, `denied` when the call did not carry the session's stand-in,
recorded at most once a minute with the log saying how many were refused
unrecorded since, `refused` when the broker would not forward it or refused its answer for the
size it declared, or `upstream-unreachable`. The method is kept only as a
standard name, the path decoded, scrubbed of stand-ins, cut at any query it
encodes, with control characters escaped, and cut to 256 bytes, a query only by
its length, since it can carry a key, and no header, body,
credential or stand-in is ever kept. A call for a provider the broker does not
serve, or one that arrives after the session stopped, leaves no receipt, only a
log line or nothing. A tunnel carries no brokered credential and leaves no
receipt, so its log lines are its only record. `errand threads show <thread>`
lists the newest calls a thread's session made. A session's receipts go when
the session is collected, as [threads](/threads) describes. A receipt that
cannot be written does not fail the call, and the log says so.

The proxy listener requires that credential. A `CONNECT` or plain `http://`
request without it receives `407 Proxy Authentication Required` with a
`Proxy-Authenticate: Basic` challenge, before any upstream connection is
opened. The session's proxy variables carry the credential as URL userinfo,
so ordinary tools send it automatically. The launcher argv still names only
the bare proxy endpoint, not the credential. This prevents a local process
that only discovers the port from using it.

The broker resolves a name itself, on this host and with this host's resolver,
not the session's, and dials only the addresses it judged. A name that resolves
only to the host's own network, loopback, private, or link-local, or to a
reserved or cloud fabric address such as Azure's `168.63.129.16`, is refused
with `403 not a public host`, and `sandbox.egress.allowInternal` lifts that only
for an address written as a literal: what a name points at is not the
operator's to decide. A name that does not resolve at all is answered with
`502 the name did not resolve`, and the log gives the resolver's reason. That
is this host's resolver to fix, `/etc/resolv.conf` first, and the daemon warns
at startup when it cannot resolve a brokered provider.

A brokered provider is held to the same rule when the broker calls it with
the real key. The address it connects to is judged as it dials, not only
before, so a name cannot answer public to the check and internal to the
connection. The broker always dials a provider directly, and the proxy
variables in the daemon's own environment play no part in it.

Its base URL must be `https://`, since the key travels on every request.
Plain `http://` is accepted only to a loopback address written as a literal,
such as `http://127.0.0.1:8080`, and that also needs
`sandbox.egress.allowInternal`. The daemon warns at startup about a provider
it will not call, and the broker refuses its calls with `502` and the
reason.

The broker follows no redirect from a provider, since a redirect would carry
the key to a host nobody judged. The redirect reaches the session as sent,
and the session cannot follow it with the stand-in it holds, so a provider
that answers with a redirect does not work behind the broker.

A provider definition says which header its key travels in with
`placement`, `"bearer"` for `Authorization: Bearer` or `"x-api-key"`. The
broker then takes the session's stand-in only in that header, answering `401`
with the header's name when it arrives in the other, and puts the real key
only there, whatever else the session sent. Left out, it follows an explicit
`kind`: `anthropic` is `x-api-key` and `openai` is `bearer`. A definition with
neither, as every provider kage already knows is, keeps the older behaviour:
the stand-in is taken in either header, and the key goes out as a bearer token
and in `x-api-key` as well when the session sent one. The image, speech and
music tools send their key as a bearer token, so a provider that carries one
of them cannot take its key in `x-api-key`. A placement must name the header
the agent's client for that provider sends, or every call is refused.

`capabilities` says which calls the key may make, as a list of method and
path, or `"*"` for every call:

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

A path is matched against what follows the provider's route, which is what is
pasted onto its base URL, so it reads as the provider's own API. It is matched
as sent, before any decoding, and never against the query. A `*` segment
matches one segment and `**` any number of them, none included. A method is
written upper case, or `*` for any. A list is a warning until
`strictCapabilities` is `true`: a call it does not name is forwarded and
named in the log. Once strict, such a call is answered `403` with the list,
and never reaches the provider. A provider with no `capabilities` forwards
every call and logs each at `info`, with the entry that would name it, so a
list can be written from a day's log. An empty list is refused, and so is
`strictCapabilities` without a list. A key that reads as a misspelling of
`placement`, `capabilities` or `strictCapabilities`, a letter or two off, in
another case, or beginning with `place`, `strict` or `capab`, as `place`,
`strictness` and `capability` do, is refused too, since a misspelt one would
leave the route open. A key that only begins alike, as
`platform` or `capacity`, is the operator's own and is ignored. Any other key errand does not
read is ignored, as a definition may carry keys for the operator's own use. The image, speech and music tools ride
the same route, so a strict list names their endpoints too, such as
`POST /images/generations` under a base URL that ends in `/v1`.

Whatever the route allows, a path that the provider, or the URL the broker
rebuilds, could read differently from how it was matched is refused with
`400`: one with an empty segment, a `.` or `..` segment, a segment that
decodes once to either or to a slash or backslash, or one carrying `;`
parameters. Headers that ask a server to run a request as another method or
at another path, such as `X-HTTP-Method-Override` and `X-Original-URL`, are
not forwarded. A framework that reads the method from a `_method` field in the
body or the query is not covered, since the broker does not read either.
Placement and capabilities are read when a session launches, so a change to
them, or to the `kind` a placement is derived from, reaches every session
launched after the reload that makes it. A tunnel is opaque
once it is open, so `sandbox.egress.allow` governs tunnels by host alone, and
only a provider route can be held to a method and a path.

The broker gives a lookup 5 seconds and the dial 10 seconds across every
address a name has, so a stalled upstream is answered with `504 the name did
not resolve in time` or `502 upstream unreachable` before a client's own
timeout fires. The log names the host and the phase that failed. A bailey
session behind the broker is locked to it alone, with UDP, ICMP, and
DNS dropped inside it, so the broker's answer and its log are the only record
of where a request stalled.

Under the bailey backend with `sandbox.egress.mode` set to `proxy`, the
default, a session runs in a private network namespace whose one way out is
its broker. With `open` and without `sandbox.hideHostAddress`, it shares the
host's network namespace, so it can read the host address, the MAC, and the
ARP neighbours through `ip`, `/proc/net`, or `/sys`. To hide them, set
`sandbox.hideHostAddress`:

```json
{
  "sandbox": {
    "hideHostAddress": true
  }
}
```

Egress then runs through a private namespace, so a session sees a synthetic
address and MAC rather than the host's. It needs `pasta` on the host; without
it the session still starts, and says the host address stays visible. The
namespace gets a synthetic IPv6 address as well where the host has one to reach,
and the ports a session may open are unchanged. The podman backend already gives
each session its own network, so it does not need this. Under the broker that
network reaches only the broker.

## Granting a host from the thread

Under `egress.mode = proxy`, a host that `sandbox.egress.allow` does not
name is refused at once with `403`, and never held while somebody decides. A
thread cannot be a dialog the agent waits on. The refusal says what to ask
for, and the thread is told once a minute for each host, and of no more than
three hosts a minute, so an agent that retries in a loop is one message a
minute rather than one an attempt:

```
this session tried to reach `api.example.com`, which is not on the egress
allowlist or granted. `!grant api.example.com` lets its next connection
through, and `!grant api.example.com 10m` or `!grant api.example.com always`
lets it through for longer
```

The owner answers in the thread, and the agent's next attempt goes through:

| command | meaning |
| --- | --- |
| `!grant <host>` | the next connection to the host the broker makes, and no more |
| `!grant <host> 10m` | any length of time, written as `retention` lengths are |
| `!grant <host> always` | the rest of the session, resumes included |
| `!grant` | list what is granted, how long is left, and by whom |
| `!revoke <host>` | drop every grant written for the host, saying so when a wildcard still covers it |

A host may be written as `*.example.com` for every name beneath it. A grant
names a host alone, never a path, since a tunnel is opaque once it is open,
and it never opens a port outside `sandbox.egressPorts` or a host-internal
address the broker refuses anyway. Every entry of `sandbox.egress.allow`,
the lone `*` of the default included, is a standing grant, so with the
default nothing is ever refused for want of one. Grants are for an operator
who narrowed the list. Each session has grants of its own, kept in its record
directory beside the state directory, where the session cannot write them. A
grant file holds hosts, times and account ids, never a credential. A
connection already open to a revoked host stays open until it ends, and a
grant for once that was never used stays until it is. Only a single exact
name is ever suggested, so a session that asks for `*.com` is refused without
a command to paste. The owner and the daemon's operators may grant, as they
may stop the session.

## Checking it on this host

The test suite checks the flags each backend is given, not what the kernel
then does. On a host where `bailey doctor` passes, a set of live checks starts
a bailey session the way the daemon does, with its arguments and its policy,
and confirms that it reaches its broker, that a host service on a port the
policy admits is still not reached directly, through the host's loopback, its
own address, or the address the broker is mapped at, since the session's
namespace and its firewall let out only the broker's port, that a port the
policy does not admit is refused at once, and that the broker's own port leads
nowhere else:

```sh
cargo test sandbox::bailey::live_tests -- --ignored --test-threads=1
```

They are left out of an ordinary run. On a host that cannot run bailey, each
check fails saying why, rather than passing.

## What it says at startup

The daemon probes the backend before it touches the chat service, and reports
what it can and cannot enforce **on this host**:

```
sandbox backend: bailey
  sessions run as confined host processes using the host's own tools
  no single file may exceed 1g, enforced as an rlimit
  1 guarantee(s) cannot be enforced on this host:
    - per-session memory, cpu, and process limits are not applied: this host
      reports no cgroup delegation
```

A gap is always stated. Presenting a weaker boundary as if it were a stronger
one is worse than the weaker boundary itself, because it takes away the chance
to decide about it.

By default a gap **stops the daemon**. Set `sandbox.requireFullEnforcement` to
`false` to run anyway, having read what is missing.

## Per-session limits

The memory, cpu, and process limits are applied per session only when the
sandbox has a cgroup it may create children in, named by `BAILEY_CGROUP_ROOT`.
Without one, the limits in your service definition still hold, but they hold
over the daemon and every session together, and the daemon reports that as the
gap above.

The [service definitions](/service) show how to provide one.

## Granting more than the default

The policy is generated per session and written to `<stateDir>/<session>/policy.toml`,
outside the project so the agent cannot rewrite it. You can read it to see
exactly what a session was given.

To let sessions reach something else, name it:

```json
{
  "sandbox": {
    "policyExtra": {
      "read": ["/opt/toolchains", "/var/cache/shared"],
      "execute": ["/opt/toolchains/bin"],
      "write": []
    }
  }
}
```

Additive only. What the daemon grants is the floor: the project and the state
directory are still placed, the environment is still built rather than
inherited, and the backend's own profile is still cleared first. Paths must be
absolute, because after the pivot there is no working directory to resolve a
relative one against.

Anything granted here is named in the startup report, and a writable grant is
called out separately, since that is the one that lets a session change
something outside its own project. A report that did not say so would describe
a tighter boundary than the one in force.

There is deliberately no way to supply a whole policy file. That would let the
report claim guarantees the file does not make; editing `src/sandbox/policy.rs`
is the honest way to change the floor itself.

## Telling a toolchain where to look

A granted path is often not enough on its own, because the environment is built
rather than inherited: a variable the daemon does not set does not exist inside
a session. `sandbox.env` names the ones that should:

```json
{
  "sandbox": {
    "env": { "CARGO_HOME": "/var/cache/errand/cargo" },
    "policyExtra": {
      "read": ["/var/cache/errand/cargo"],
      "write": ["/var/cache/errand/cargo"],
      "execute": []
    }
  }
}
```

That is the shape of a cache shared between sessions and kept off your own:
sessions warm one directory that is nobody's real cache, rather than
redownloading into a state directory that is thrown away with the session.
Setting the variable and granting the path are two steps on purpose, since a
variable does not widen the boundary and a grant does.

A granted directory is still not on a session's PATH, so a program in it is
found only by its full path. `sandbox.pathExtra` puts it there:

```json
{
  "sandbox": {
    "pathExtra": ["/opt/toolchains/bin"],
    "policyExtra": {
      "read": ["/opt/toolchains"],
      "execute": ["/opt/toolchains/bin"],
      "write": []
    }
  }
}
```

Those directories sit after the agent's own wrappers and ahead of `/usr/bin`,
so a toolchain named on purpose is the one a session finds rather than the
host's copy. Naming one grants nothing: a directory that is not also granted is
a name on a path leading nowhere.

`PATH` and `HOME` are refused, because the policy sets both to paths it places,
and so is the name carrying the provider credential. A name the daemon sets
itself keeps the daemon's value, so nothing here can decide what the agent
authenticates as. The names given are reported at startup; the values are not,
and a value reaches the agent, so nothing secret belongs here.

## What is not confined

- **The daemon itself.** It holds the chat token and starts sandboxes.
- **The web interface.** Anyone who can reach it acts with operator authority,
  which is why the address it binds to is checked and a public bind refused.
- **Disk use**, which is measured rather than enforced: no backend caps what a
  process tree writes in aggregate without a sized filesystem under it. A
  session that passes its budget is stopped, and the check paces itself against
  how fast the session is writing.

## What the agent holds

The provider credential, because it needs it, and a GitHub token when one is
configured, because reading issues and checking builds is most of working on
somebody's repository. Under `egress.mode = proxy` the provider credential is
its broker's stand-in, on either backend. Under `open` the real value is held.
The GitHub credential is the daemon's own token, unless `github.app` is set,
when it is an installation token minted for the session, narrowed to the
App's installation and renewed within the hour, as [GitHub](/github)
describes. Under podman the provider credential, and the daemon's GitHub
token when there is no App, are in the container's environment, named on the
`podman run` command line but never written there. An App token is kept in the state directory instead,
since a renewed one could not reach an environment fixed at launch.

It does not hold the chat token. The text a session reports is scrubbed of
every configured secret of 8 bytes or more, and of every stand-in its broker
minted, before it reaches a channel, a browser, or the transcript on disk. An
upload, a file pulled with `!file` or a picture or clip a session or a
channel command made, is bytes rather than text and is not scrubbed. One that
holds such a secret is refused whole, and whoever asked is told why. In a
session the house rules text counts as well. A provider credential kept as a
placeholder, such as `not-needed`, counts too, so a file that merely contains
that word is refused.

A secret shorter than 8 bytes is neither scrubbed nor checked for, since
replacing a string that short wherever it occurs mangles ordinary prose. The
chat and GitHub tokens are refused at load when they are that short. A
provider credential is not, because a short one may be a deliberate
placeholder for a local server, so a provider credential under 8 bytes has no
protection here at all.

That is damage control on an unavoidable exposure rather than a boundary. An
agent that re-encodes a key, or splits it, defeats it.

A brokered provider's answers are scrubbed on the way back, too. The broker
puts the real credential on each request, so an upstream that quotes what it
was sent, as a verbose error page might, would otherwise hand the session the
key. Every occurrence of that provider's credential is replaced with
`[redacted]` in the body, as it streams, and in every header value. To read
the bytes at all, the broker asks the provider for an uncompressed answer. One
that arrives compressed anyway is refused with `502`, and the log says so,
since bytes the broker cannot scrub could carry the key out. An answer with
no body is relayed whatever coding it names. A provider that compresses
regardless of being asked not to fails every call this way, each with the
warning `a provider answered compressed though asked not to`, and cannot be
used behind the broker. An echo that re-encodes the key, as JSON escapes, URL encoding, or base64,
is not caught either, nor one that quotes only part of it. An answer over
64 MiB is refused when it says so up front, and cut when it streams past that.
