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

Both hold a session to one project directory and one state directory, and give
it network access only to reach the model provider.

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
report claim guarantees the file does not make; editing `src/sandbox/policy.ts`
is the honest way to change the floor itself.

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
somebody's repository.

It does not hold the chat token, and everything a session reports is scrubbed of
every configured secret before it reaches a channel, a browser, or the
transcript on disk. That is damage control on an unavoidable exposure rather
than a boundary: an agent that re-encodes a key defeats it.
