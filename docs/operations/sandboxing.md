# Sandboxing

Every session runs confined, and there is no way to turn that off. The agent is
the untrusted party here. It runs commands a model wrote, following
instructions from whoever can post in the channel.

## Backends

| Backend | How it confines | Use it when |
| --- | --- | --- |
| **bailey** (default) | A host process under Landlock, seccomp, and user, PID and UTS namespaces. | You want sessions to use the host's own tools, with no image to build. |
| **podman** | A rootless container from an image you provide. | You want sessions to see a filesystem you assembled. |
| **incus** | An unprivileged system container with its own init. | Sessions need Docker, podman, a database, FUSE, or systemd inside. |

```json
{ "sandbox": { "backend": "podman", "image": "localhost/errand-agent" } }
```

Incus shares the host kernel, so a kernel bug reachable from the guest is a
host problem, and the startup report says so. It also needs host setup, covered
in [the Incus backend](/operations/incus).

Under every backend, a session sees one project directory and one state
directory. What it can reach on the network depends on `sandbox.egress.mode`,
covered in [network egress](/operations/egress).

### How kage is configured

At startup errand asks the kage that sessions will run which providers it
knows: the host's kage under bailey, or the image's under podman and Incus. A
provider kage knows is written into its configuration as an override, keeping
kage's own models. Any other is written as a custom provider. A provider id
kage reserves, such as `acp`, is refused at startup and on every reload, and a
kage too old to answer stops the daemon.

The agent's own path guard is turned off in the configuration errand writes,
because the backend is already the boundary.

## The startup report

Before connecting to the chat service, the daemon probes the backend and
reports what it can and cannot enforce on this host:

```
sandbox backend: bailey
  sessions run as confined host processes using the host's own tools
  no single file may exceed 1g, enforced as an rlimit
  1 guarantee(s) cannot be enforced on this host:
    - per-session memory, cpu, and process limits are not applied: this host
      reports no cgroup delegation
```

**Any gap stops the daemon by default** (exit code 3). After reading what is
missing, you can set `sandbox.requireFullEnforcement` to `false` to run anyway.
Extra grants from `policyExtra` are listed in the report too, with writable
grants called out separately.

## Per-session limits

Memory, CPU, and process limits apply to each session only when the sandbox has
a cgroup it may create children in, named by `BAILEY_CGROUP_ROOT`. Without one,
the limits in your service definition still hold, but over the daemon and every
session together, and the report lists that as a gap.
[Running as a service](/operations/service#resource-limits) shows how to set
one up.

Disk use is measured rather than enforced. A session that goes over its budget
is stopped, and the check runs more often the faster the session is writing.

## Granting more access

The policy is generated per session and written to
`<stateDir>/<session>/policy.toml`, outside the project so the agent cannot
change it. Read it to see exactly what a session was given.

To let sessions reach more of the host, add paths under `policyExtra`:

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

Grants only add to the default. Paths must be absolute. There is no way to
supply a whole policy file, so the report always describes the policy in force.
To change the floor itself, edit `src/sandbox/policy.rs`.

`policyExtra` and `pathExtra` do not apply under Incus, where the guest image
carries its own tools.

### Environment variables

A session's environment is built from scratch, so a variable the daemon does not
set does not exist inside it. Use `sandbox.env` to add some. A shared cache is a
common reason:

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

Setting a variable grants nothing, so the path needs its own grant.

- `PATH`, `HOME`, and the variable carrying the provider credential are refused.
- A variable the daemon sets itself keeps the daemon's value.
- Names are listed at startup, values are not, and every value is blanked in
  the log. The values do reach the agent, though, so do not put secrets here.

### Extra directories on PATH

A granted directory is not on the session's PATH. `sandbox.pathExtra` adds it,
after the agent's own wrappers and before `/usr/bin`:

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

A directory on `pathExtra` that is not also granted is unreachable.

## What the agent holds

| Credential | Held by the session? |
| --- | --- |
| Chat token | Never. |
| Provider key | A per-session stand-in when brokered (the default). The real key under `egress.mode = open`. |
| Forge token | The daemon's token, or a short-lived installation token with a [GitHub App](/guide/forges#scoped-tokens-with-a-github-app). |

Under podman, the provider key and the daemon's forge token are passed in the
container's environment. They are named on the `podman run` command line, but
their values are never written there. An App token is kept in the state
directory instead, so a renewed one reaches the session.

### Scrubbing

Text a session reports is scrubbed of every configured secret of 8 bytes or
more, and of every stand-in its broker minted, before it reaches the channel,
the web interface, or the transcript on disk. Uploads are bytes rather than
text, so they are not scrubbed. An upload containing a secret is refused whole,
and whoever asked is told why. House rules count as a secret here, and so does
a placeholder provider credential such as `not-needed`.

A secret shorter than 8 bytes is neither scrubbed nor checked, because
replacing a string that short would mangle ordinary text. Chat and forge tokens
that short are refused at load. A provider credential is not, because a short
one may be a deliberate placeholder for a local server, so it has no
protection here.

::: warning Damage control, not a boundary
An agent that re-encodes or splits a key defeats scrubbing. The broker keeping
the real key out of the sandbox is what protects it.
:::

## What is not confined

- **The daemon.** It holds the chat token and starts the sandboxes.
- **The web interface.** Without a login, anyone who can reach it acts as the
  operator, which is why its bind address is checked.

## Live checks

The test suite checks the flags each backend is given, not what the kernel then
does. On a host where `bailey doctor` passes, a set of live checks starts a real
bailey session the way the daemon does, and confirms it can reach its broker
and nothing else:

```sh
cargo test sandbox::bailey::live_tests -- --ignored --test-threads=1
```

On a host that cannot run bailey, each check fails and says why.
