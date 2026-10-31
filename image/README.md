# The errand guest image

The image the Incus backend starts a session from. It is a Debian system
container with an init, the agent, Docker, rootless podman, FUSE and the
wrapper the backend enters through.

The backend consumes an alias, not this directory. An externally built image
is fine if it carries the same contract property.

## What the image provides

| item | value |
| ---- | ----- |
| init | systemd, PID 1 |
| agent | `/usr/local/bin/kage`, pinned by whoever builds the image |
| wrapper | `/usr/local/bin/errand-exec` |
| agent uid and gid | 1000 |
| Rust | rustup stable, clippy, rustfmt |
| Go | Debian `golang-go`, `go` and `gofmt` on PATH |
| jj | Jujutsu 0.46.0, static musl build in `/usr/local/bin/jj` |
| GitHub CLI | `gh` from Debian |
| Bun | `/usr/local/bun/bin/bun` |
| Python | `python3`, `pip3`, `venv` |
| Node.js | `nodejs` 20.19.2, `npm` 9.2.0 |
| uv | `uv` and `uvx` in `/usr/local/bin` |
| Zig | latest stable at build time, `/usr/local/bin/zig` |
| cross | Rust `cross` in `/usr/local/cargo/bin`, podman engine by default |
| compilers | gcc from `build-essential`, `clang`, `lld` |
| QEMU | `qemu-system-x86`, `qemu-utils` |
| Xvfb | `xvfb`, `xvfb-run` |
| GUI | Xvfb, openbox, xdotool, `scrot`, ImageMagick, Mesa llvmpipe |
| GUI launcher | `/usr/local/bin/errand-gui` |
| media | `ffmpeg` 7.1.5 and `ffprobe` |
| fonts | `fonts-dejavu-core` |
| utilities | `git`, `curl`, `wget`, `vim`, `less`, `htop`, `jq`, `rg`, `fd`, `fastfetch`, `file` |
| sudo | the agent has passwordless sudo inside the guest |
| docker socket group | `agent` (gid 1000) |
| rootless podman | host networking, cgroupfs manager |
| rootless podman subids | `agent:1001:64535` |
| workspace mount point | `/workspace` |
| state mount point | `/state` |
| agent home | `/state/home` |

The backend sets the session limits, the idmap, the security keys and the
tmpfs sizes. The image does not know any of them.

## The contract property

The backend reads `user.errand.contract` from the image alias, or from the
golden instance's config when one is configured. The value is one JSON object:

```json
{
  "version": 1,
  "guestUid": 1000,
  "dockerGid": 1000,
  "kageProviders": { "providers": [ ... ] }
}
```

`kageProviders` is exactly what `kage providers --json` prints. The backend
writes the agent's provider configuration from it, so an image without the
property is refused rather than guessed at.

The build script runs `kage providers --json` inside the image and sets the
property in the same step that installs kage, so the two do not drift.

## Build

The script builds the image with Incus itself, on the host the backend runs
on. It needs a kage binary for the guest, which must be newer than 0.5.1:

```sh
KAGE_BIN=/path/to/kage ./build.sh
```

Optional settings:

| variable | default | what it does |
| -------- | ------- | ------------ |
| `INCUS_PROJECT` | `errand` | the project the image is published in |
| `INCUS_ALIAS` | `errand-agent` | the image alias the backend names |
| `INCUS_GOLDEN` | none | when set, also create a stopped instance of that name |

The script creates a temporary container named `errand-image-build`, installs
the packages, pushes the wrapper, the socket drop-in and kage, enables Docker,
stops the container and publishes it. The temporary container is deleted on
exit.

The published image is not bit-for-bit reproducible. It is built from the
Debian image server and the packages available at build time. Record the
image fingerprint beside a deployment if that matters.

## The wrapper

`errand-exec` reads `/run/errand/env`, one `NAME=VALUE` per line, exports each
value without evaluating it, and removes the file. The directory is owned by
the agent uid, so the wrapper can remove the file the backend wrote as root.
The empty directory stays in the tmpfs until the instance is deleted. When the
environment names
a broker socket and a relay port, it starts `socat` on that loopback port and
forwards to the socket, then execs the agent.

The backend writes the file as root, mode 0600, on the guest's tmpfs, and
runs the wrapper as the agent uid. The agent's stdin is the protocol, never
the environment file.

## Rust

The image installs the stable Rust toolchain with rustup. The toolchain is
under `/usr/local/rustup`, the proxies (`cargo`, `rustc`, `rustup`, `clippy`,
`rustfmt`) are in `/usr/local/cargo/bin`, and `build-essential`, `pkg-config`
and `libssl-dev` are present for crates that link C.

The wrapper puts `/usr/local/cargo/bin` ahead of `PATH` and sets
`RUSTUP_HOME=/usr/local/rustup`. Cargo's own home is not set, so it uses
`$HOME/.cargo`, which is inside the session state and persists with it. A
session can therefore build, cache and reuse dependencies without downloading
a toolchain per launch.

## Go and Bun

Go comes from Debian's `golang-go` package. Its `go` and `gofmt` are linked
into `/usr/local/bin` when the package does not put them on the default PATH.
`GOPATH` defaults to `$HOME/go` and `GOCACHE` to `$HOME/.cache/go-build`,
both inside the session state.

Bun is installed from its own installer under `/usr/local/bun`, not from
Debian. The wrapper adds `/usr/local/bun/bin` ahead of `PATH`. Bun's caches
also follow `HOME`, so they persist with the session.

## Node, uv, Zig and cross

Node.js and npm come from Debian. npm's global prefix is the system one, so a
global install needs sudo; project-local installs do not.

uv and Zig are not packaged in Debian trixie. The build installs uv from its
installer and Zig from the latest stable tarball, both under `/usr/local` so
every session sees them. uv keeps its cache and managed Pythons under the
agent's home, inside the session state.

`cross` is installed with `cargo install` into `/usr/local/cargo/bin`. The
wrapper sets `CROSS_CONTAINER_ENGINE=podman` unless the operator overrides it,
because rootless podman is the container engine that works in proxy mode and
carries the session's proxy. The cross images are pulled by that engine.

gcc comes from `build-essential`; clang and lld are separate. QEMU's system
emulator and image tools, Xvfb, and `file` are installed for workloads that
need them. The guest has no `/dev/kvm` unless the operator passes one through
the project profile. When it is passed, a systemd unit widens the node to
0666 at boot, so a session can use hardware acceleration. Without it, a guest
VM uses TCG and is slow.

## Jujutsu and GitHub

Jujutsu is not packaged in Debian trixie. The build takes the static musl
release tarball and installs one binary at `/usr/local/bin/jj`. The version is
pinned in `build.sh`, because the release asset name carries the tag. jj needs
git 2.42 or newer, which trixie satisfies. It works in colocated repositories,
so `jj root` succeeds in a git checkout.

`gh` comes from Debian. It carries no credentials; a session authenticates
with `gh auth login` or `GH_TOKEN` when it needs GitHub.

## Headless GUI

The guest has no display, no GPU, no sound device and no input device. The
image carries userspace stand-ins that need no kernel module:

| lack | the image has |
| ---- | ------------- |
| display | Xvfb, X11 clients and `x11-utils` |
| window manager | openbox |
| keyboard and mouse | xdotool over XTEST, inside X only |
| GPU | Mesa llvmpipe with `LIBGL_ALWAYS_SOFTWARE=1` |
| screenshots | `scrot`, ImageMagick `import` and `compare` |
| session bus | `dbus-launch` from `dbus-x11` |
| clipboard | `xclip` |
| window control | `wmctrl` |

`errand-gui` starts Xvfb on `:99` at 1920x1080x24, openbox and a session bus,
then prints the exports. It keeps its state in `/tmp/errand-gui-<uid>`, so a
later command in a fresh shell reads the same display:

```sh
eval "$(errand-gui)"
xdotool mousemove 10 20 getmouselocation
scrot /tmp/shot.png
```

`errand-gui --file` prints the env file, and `errand-gui --stop` kills the
display, the window manager and the bus. The display and the window manager
are detached with `setsid`, so they survive the shell that started them.

What is deliberately not in the image: browsers, tesseract, an audio
server, the accessibility stack and GTK or Qt development headers. They are
large, they change often, and a session can install them with
`sudo apt-get install` through the broker in proxy mode. Browsers for
Playwright and Puppeteer are better fetched by the project than baked in.

ffmpeg is in the image, because recording a display and transcoding media are
common enough to justify it. It adds 95 packages and about 172 MiB installed,
by far the largest single group here.

OpenGL runs on llvmpipe. `glxinfo -B` reports the renderer. There is no
`/dev/dri`, no `/dev/snd`, no `/dev/uinput` and no `/dev/video*`, so anything
that needs the kernel side of those devices cannot work here. A camera has no
userspace stand-in; feed a file or a synthetic stream instead.

## Docker access without a supplementary group

`incus exec` sets a uid and a gid, not a supplementary group list. The image
gives the agent's primary gid access to the Docker socket instead, with a
`docker.socket` drop-in that sets `SocketGroup=agent`. An image that leaves
the socket in a separate group will run the agent without Docker access.

## Rootless podman

The outer container's idmap covers guest ids 0 to 65535. A rootless podman
inside the guest therefore needs subids inside that range, not the host
default of 100000. The build script writes `agent:1001:64535` to the guest's
`/etc/subuid` and `/etc/subgid`.

`/etc/containers/containers.conf` sets the default network to host and the
cgroup manager to cgroupfs. The host network is deliberate: under
`egress.mode=proxy` the guest has no NIC, and pasta fails with
`Couldn't set IPv4 route(s) in guest: Invalid argument` when it tries to set
up a route in that empty namespace. A container on the guest's host network
shares the guest's loopback, so it reaches the broker relay at
`127.0.0.1:3128`, and podman passes the guest's proxy variables into the
container by default. cgroupfs avoids the systemd user session warning, since
no login session exists for a uid that `incus exec` starts directly.

The default is a starting point, not a law. `--network=slirp4netns` and
`--network=none` still work. A container that needs the broker must use host
networking, because a slirp or pasta namespace has its own loopback and cannot
see the relay.

Podman uses cgroupfs and Docker uses systemd. They are separate runtimes and
the drivers do not have to match. Podman can still print a warning about
adding its pause process to a systemd sandbox cgroup; it is noise, and the
container starts.

`neofetch` is not in Debian trixie. The image installs `fastfetch` instead.

## Docker in proxy mode

Docker's default bridge has no uplink when the guest has no NIC. A container
that needs the broker must use `--network=host` as well. The Docker daemon
itself does not receive the session's proxy variables, so a `docker pull`
under `egress.mode=proxy` has no route unless the daemon is configured with a
proxy separately. Podman pulls client-side with the session's proxy and is the
simpler tool in this mode.

## Package installation

The agent has `NOPASSWD` sudo inside the guest. Guest root is root inside the
guest, and the boundary is the container, so installing packages is allowed.
Files written through sudo belong to guest root, not to the host account; the
agent should write workspace files as its own uid. The host ownership check
covers that.

The apt lists and archives stay root-owned. Running `apt-get` as the agent uid
without sudo fails by design; the fix is `sudo`, not loosening the apt
directories. The sudoers rule keeps `HTTP_PROXY`, `HTTPS_PROXY` and their
lowercase forms, so `sudo apt-get` and `sudo pip3` reach the broker in proxy
mode.

`pip3` is Debian's externally managed Python. Package installs belong in a
`python3 -m venv`, not in the system interpreter. `pip3 --version` and venv
creation work as the agent.

## Direct DNS and ICMP

Under `egress.mode=proxy` the guest has no NIC. The broker resolves names for
proxied HTTP and HTTPS, and a container on the host network uses the same
proxy variables. A direct resolver or `ping 8.8.8.8` is not available in that
mode and should not be worked around by hardcoding a public nameserver; the
address is unreachable and bypasses the host's resolver in open mode. In open
mode the profile NIC provides DNS through the host's resolver.

## What the acceptance uses

The acceptance checks in the issue run against this image, not an arbitrary
one: Docker build and run, rootless podman, a FUSE mount, a systemd service,
host ownership of `/workspace`, the tmpfs sizes, and the environment file
landing in the agent.
