# The Incus backend

The Incus backend runs each session in an unprivileged system container. The
guest has its own init and package manager, so Docker, podman, databases, FUSE,
and systemd services work inside it. It shares the host kernel, and the startup
report says so.

## Host setup

The daemon runs as an ordinary account. For Incus that account must be in
`incus-admin`, because the restricted per-user `incus` group cannot grant the
project settings below:

```sh
sudo adduser errand incus-admin
```

Create the project sessions live in, allowing only what the backend needs:

```sh
incus project create errand
incus project set errand restricted=true
incus project set errand restricted.containers.nesting=allow
incus project set errand restricted.containers.lowlevel=allow
incus project set errand restricted.containers.privilege=unprivileged
incus project set errand restricted.devices.disk=allow
incus project set errand restricted.devices.disk.paths=/srv/errand,/var/lib/errand
incus project set errand restricted.devices.proxy=block
incus project set errand restricted.idmap.uid=1000
incus project set errand restricted.idmap.gid=1000
incus profile device add default root disk path=/ pool=default --project errand
incus profile device add default eth0 nic network=incusbr0 --project errand
```

- `restricted.devices.disk.paths` must cover `projectRoot` and `stateDir`. The
  broker's socket sits beside each session's state directory, so the state root
  covers it.
- The idmap values are the uid and gid that own those directories, which are
  the daemon account's. Use your own values rather than `1000` if they differ.

Both ids also need an allocation for `root`, or `newuidmap` refuses the
instance at start:

```sh
echo 'root:1000:1' | sudo tee -a /etc/subuid
echo 'root:1000:1' | sudo tee -a /etc/subgid
```

The startup probe reports a missing allocation rather than failing at the first
launch.

### KVM (optional)

QEMU inside a session can use hardware acceleration when the host has
`/dev/kvm` and the project passes it through. This widens the attack surface,
since a KVM bug is a way out of the container.

```sh
incus project set errand restricted.devices.unix-char=allow
incus profile device add default kvm unix-char path=/dev/kvm --project errand
```

The runtime creates the node as `0660 root:kvm`, which the agent's uid cannot
open, and the device's `gid` and `mode` options are not applied at start. The
image fixes the mode at boot with a systemd unit instead. Without the device,
QEMU falls back to TCG.

## The image

The `image/` directory in the repository builds the guest image. It carries
what a session usually needs, including Rust, Go, Bun, Node.js, uv, Zig,
cross, gcc, clang, QEMU, Xvfb with openbox and xdotool, ImageMagick, jj, `gh`,
ffmpeg, Docker, and podman. `image/README.md` describes the build.

An image must carry a `user.errand.contract` property naming its contract
version and the providers its kage knows. The probe refuses an image without
one.

## Configuration

```json
{
  "sandbox": {
    "backend": "incus",
    "incus": {
      "project": "errand",
      "image": "errand-agent",
      "rootSize": "20g",
      "guestUid": 1000
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `project` | The Incus project created above. |
| `image` | The image alias sessions start from. This is separate from `sandbox.image`, which is the podman image. |
| `rootSize` | The root disk size, on a storage pool that enforces quotas. |
| `guestUid` | The uid inside the guest that the host ids map to. |
| `golden` | Optional. A stopped instance to copy for each session instead of creating one from the image. It must carry the same contract property, and must not carry `user.errand.system`, or the orphan sweep deletes it. |

## Network

| `network` | `egress.mode` | Result |
| --- | --- | --- |
| `none` | any | No NIC, no broker, no network. |
| `restricted` | `proxy` | No NIC. The session reaches its broker through a read-only Unix socket mount and a relay on the guest's loopback. |
| `restricted` | `open` | The profile NIC stays and the session reaches the network directly. The host allowlist is not enforced, and the report says so. |

The broker's socket is a sibling of the session's state directory, never inside
it, because the agent can write its state directory.

In proxy mode there is no direct DNS or ICMP. The broker resolves names for
proxied HTTP and HTTPS, so do not hard-code a public nameserver. The agent has
passwordless sudo in the guest, and the proxy variables survive `sudo`, so apt
and pip use the broker too. Use sudo rather than loosening the root-owned apt
directories.

### Containers inside a session

The image sets rootless podman's default network to the guest's host network.
Without a NIC, pasta cannot set up routes, so a default podman container would
fail. On the host network it shares the guest's loopback, reaches the broker
relay, and gets the session's proxy variables.

Docker keeps its default bridge, which has no uplink in proxy mode. A Docker
container that needs the network should use `--network=host`, and the Docker
daemon needs its own proxy configuration for pulls.

## Limits

| Configuration | Incus setting |
| --- | --- |
| `sandbox.memory` | `limits.memory`, with `limits.memory.swap=false` |
| `sandbox.cpus` | `limits.cpu.allowance` as a time quota, with `limits.cpu` unset |
| `sandbox.pids` | `limits.processes` |
| `sandbox.fileMax` | `limits.kernel.fsize`, in bytes |
| `sandbox.gracePeriodMs` | `incus stop --timeout`, rounded up to whole seconds |
| `sandbox.incus.rootSize` | The root disk size |
| `sandbox.tmpSize`, `sandbox.shmSize` | Fresh tmpfs mounts at `/tmp` and `/dev/shm` |
| `sandbox.diskTmp` | A disk device from the state directory at `/tmp` |

These settings do nothing under Incus and are reported as inert:
`sandbox.image`, `sandbox.hideHostAddress`, `sandbox.policyExtra`, and
`sandbox.pathExtra`. `sandbox.egressPorts` is inert under `egress.mode = open`.
