#!/bin/sh
# Builds the errand guest image with Incus itself, then optionally a golden.
#
# The build runs in a temporary container on the same Incus daemon the
# backend uses, so the image it publishes is the one the host will start.
set -eu

: "${KAGE_BIN:?set KAGE_BIN to a kage binary newer than 0.5.1}"

project=${INCUS_PROJECT:-errand}
alias=${INCUS_ALIAS:-errand-agent}
golden=${INCUS_GOLDEN:-}
name=errand-image-build
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

cleanup() {
    incus delete --force "$name" --project "$project" >/dev/null 2>&1 || true
}
trap cleanup EXIT

incus delete --force "$name" --project "$project" >/dev/null 2>&1 || true
incus launch images:debian/13 "$name" --project "$project" </dev/null

incus exec "$name" --project "$project" -- sh -s <<'SCRIPT'
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    systemd-sysv docker.io docker-cli docker-buildx podman fuse3 socat passt \
    ca-certificates git iproute2 uidmap slirp4netns fuse-overlayfs \
    build-essential pkg-config libssl-dev curl unzip golang-go sudo \
    python3 python3-pip python3-venv vim less htop jq ripgrep fd-find \
    wget fastfetch nodejs npm clang lld qemu-system-x86 qemu-utils gh \
    ffmpeg \
    xvfb x11-utils x11-xserver-utils xdotool openbox dbus-x11 \
    fonts-dejavu-core mesa-utils libgl1-mesa-dri scrot xterm wmctrl xclip \
    imagemagick file xz-utils
groupadd --gid 1000 agent 2>/dev/null || true
useradd --uid 1000 --gid 1000 --home-dir /state/home --shell /bin/bash \
    --no-create-home agent 2>/dev/null || true
install -d -o 1000 -g 1000 -m 0755 /state/home /workspace
# The runtime creates /dev/kvm as 0660 root:kvm, and the agent is uid 1000.
# systemd-tmpfiles skips /dev, so a small unit widens the node instead. Without
# the device the condition fails and the unit is skipped.
cat > /etc/systemd/system/errand-kvm.service <<'UNIT'
[Unit]
Description=Widen /dev/kvm for the agent
ConditionPathExists=/dev/kvm

[Service]
Type=oneshot
ExecStart=/bin/chmod 0666 /dev/kvm
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
# The agent is confined by the container, not by its guest uid. Guest root is
# root inside the guest, so package installation is allowed to work. The proxy
# variables survive sudo so apt and pip use the broker in proxy mode.
printf "Defaults env_keep += \"HTTP_PROXY HTTPS_PROXY http_proxy https_proxy NO_PROXY no_proxy\"\nagent ALL=(ALL) NOPASSWD: ALL\n" > /etc/sudoers.d/agent
chmod 0440 /etc/sudoers.d/agent
visudo -cf /etc/sudoers.d/agent
# Debian ships fd as fdfind; the agent expects the usual name.
if ! command -v fd >/dev/null 2>&1 && [ -x /usr/bin/fdfind ]; then
    ln -sf /usr/bin/fdfind /usr/local/bin/fd
fi
# uv and Zig are not packaged in trixie. Both go under /usr/local so every
# session sees them.
curl -LsSf https://astral.sh/uv/install.sh -o /tmp/uv-install.sh
UV_INSTALL_DIR=/usr/local/bin UV_UNMANAGED_INSTALL=/usr/local/bin sh /tmp/uv-install.sh
rm -f /tmp/uv-install.sh
chmod -R a+rX /usr/local/bin/uv /usr/local/bin/uvx
uv --version
uvx --version
zig_index=$(curl -fsSL https://ziglang.org/download/index.json)
zig_version=$(printf "%s" "$zig_index" | jq -r 'to_entries | map(select(.key != "master")) | .[0].key')
zig_url=$(printf "%s" "$zig_index" | jq -r --arg v "$zig_version" '.[$v]["x86_64-linux"].tarball')
curl -fsSL "$zig_url" -o /tmp/zig.tar.xz
mkdir -p /usr/local/zig
tar -xJf /tmp/zig.tar.xz -C /usr/local/zig --strip-components=1
ln -sf /usr/local/zig/zig /usr/local/bin/zig
rm -f /tmp/zig.tar.xz
chmod -R a+rX /usr/local/zig
zig version
# Rootless podman inside this guest must stay inside the outer container map,
# which covers guest ids 0..65535. 1000 is the agent, so its subids start at
# 1001 rather than at the host default of 100000.
grep -q "^agent:" /etc/subuid || echo "agent:1001:64535" >> /etc/subuid
grep -q "^agent:" /etc/subgid || echo "agent:1001:64535" >> /etc/subgid
# The Rust toolchain lives in the image, not in the session state, so every
# session gets the same one and it is not downloaded per session. The proxies
# in /usr/local/cargo/bin read RUSTUP_HOME, which the wrapper sets.
mkdir -p /usr/local/rustup /usr/local/cargo
curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal \
        --default-toolchain stable
RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    /usr/local/cargo/bin/rustup component add clippy rustfmt
rm -f /tmp/rustup-init.sh
chmod -R a+rX /usr/local/rustup /usr/local/cargo
# Go comes from Debian. Its go command is not always on the default PATH, so
# both it and gofmt are linked where the session PATH already looks.
if ! command -v go >/dev/null 2>&1; then
    go_bin=$(find /usr/lib -maxdepth 4 -type f -path "*/bin/go" | head -n 1)
    if [ -n "$go_bin" ]; then
        ln -sf "$go_bin" /usr/local/bin/go
        ln -sf "$(dirname "$go_bin")/gofmt" /usr/local/bin/gofmt
    fi
fi
# jj is not packaged in trixie. The musl build is static, so it does not
# depend on the guest's libc. Bump the version here; the asset name carries
# the tag.
jj_version=v0.46.0
curl -fsSL \
    "https://github.com/jj-vcs/jj/releases/download/$jj_version/jj-$jj_version-x86_64-unknown-linux-musl.tar.gz" \
    -o /tmp/jj.tar.gz
tar -xzf /tmp/jj.tar.gz -C /usr/local/bin ./jj
chmod 0755 /usr/local/bin/jj
rm -f /tmp/jj.tar.gz
jj --version
# Bun publishes a self-contained binary, not a Debian package. It goes under
# /usr/local so the session account can run it.
curl --proto "=https" --tlsv1.2 -sSf https://bun.sh/install -o /tmp/bun-install.sh
BUN_INSTALL=/usr/local/bun bash /tmp/bun-install.sh
rm -f /tmp/bun-install.sh
chmod -R a+rX /usr/local/bun
export PATH=/usr/local/bun/bin:/usr/local/cargo/bin:$PATH
export RUSTUP_HOME=/usr/local/rustup
export CARGO_HOME=/usr/local/cargo
go version
bun --version
cargo --version
rustc --version
# The Rust cross tool uses the guest's rootless podman by default.
RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    /usr/local/cargo/bin/cargo install cross --locked
chmod -R a+rX /usr/local/cargo
/usr/local/cargo/bin/cross --version
# A published image is cloned, so the machine id must be regenerated by each
# guest rather than shared by all of them.
: > /etc/machine-id
rm -f /var/lib/dbus/machine-id
rm -rf /tmp/* /var/tmp/*
SCRIPT

incus file push "$here/errand-exec" "$name/usr/local/bin/errand-exec" \
    --mode 0755 --project "$project" </dev/null
incus file push "$here/errand-gui" "$name/usr/local/bin/errand-gui" \
    --mode 0755 --project "$project" </dev/null
incus file push "$KAGE_BIN" "$name/usr/local/bin/kage" \
    --mode 0755 --project "$project" </dev/null
incus file push "$here/docker-socket.conf" \
    "$name/etc/systemd/system/docker.socket.d/errand.conf" \
    --create-dirs --project "$project" </dev/null
incus file push "$here/containers.conf" \
    "$name/etc/containers/containers.conf" \
    --create-dirs --project "$project" </dev/null

incus exec "$name" --project "$project" -- systemctl enable docker.socket docker.service errand-kvm.service </dev/null

providers=$(incus exec "$name" --project "$project" -- kage providers --json </dev/null)
contract=$(printf '{"version":1,"guestUid":1000,"dockerGid":1000,"kageProviders":%s}' "$providers")

incus stop "$name" --project "$project" </dev/null
incus publish "$name" --alias "$alias" --reuse --project "$project" </dev/null
incus image set-property "$alias" user.errand.contract "$contract" \
    --project "$project" </dev/null

if [ -n "$golden" ]; then
    incus delete --force "$golden" --project "$project" >/dev/null 2>&1 || true
    incus copy "$name" "$golden" --project "$project" </dev/null
    incus config set "$golden" user.errand.contract "$contract" \
        --project "$project" </dev/null
fi

echo "built image $alias in project $project"
if [ -n "$golden" ]; then
    echo "golden $golden is ready"
fi
