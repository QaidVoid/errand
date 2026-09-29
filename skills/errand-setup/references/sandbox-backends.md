# Sandbox backends

Errand confines every session. There is no unsandboxed mode, so one of these
must be on the host or the daemon refuses to start. `sandbox.backend` picks
which one, and defaults to `bailey`.

## bailey

Confines a session as a host process with Landlock, seccomp, and namespaces.
Sessions use the host tools, so there is no image to build.

It needs:

- The `bailey` tool on PATH.
- The `pi` agent on PATH. This backend runs the host install, so a host
  without it is refused at startup rather than at the first session.
- A kernel with Landlock and user namespaces. `bailey doctor` says what the
  host enforces.

Pick bailey when the host already has the toolchain sessions should use and
no container image should be maintained.

## podman

Runs each session in a rootless container from an image the operator
provides. It needs:

- Rootless podman. A podman that is not rootless is refused at startup.
- The configured image present on the host before starting. A missing image
  is reported at startup rather than at the first session.

Pick podman when sessions should see an assembled filesystem instead of the
host. See `docs/sandboxing.md` for what the container flags enforce.

## Network

Both backends hold a session to one project directory and one state
directory, and give it network access only to reach the model provider over
HTTPS. `sandbox.egressPorts` names the outbound ports and defaults to
`[443]`. Under `sandbox.egress.mode` set to `proxy`, a broker stands in
front of the providers. A session with `network` set to `none` opens
nothing and cannot reach a model provider.

Under bailey a session shares the host network namespace, so it can read the
host address. `sandbox.hideHostAddress` routes egress through a private
namespace instead and needs `pasta` on the host.

## Enforcement gaps

The daemon probes the backend before touching the chat service and reports
what it cannot enforce on this host. By default a gap stops the daemon. Set
`sandbox.requireFullEnforcement` to `false` to run anyway, having read what
is missing. Per-session memory, cpu, and process limits additionally need a
cgroup the sandbox may create children in, named by `BAILEY_CGROUP_ROOT`.
Without one the daemon reports the gap rather than implying limits it does
not apply.
