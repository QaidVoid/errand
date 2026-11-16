#!/bin/sh
# Provisions the errand guest image. Runs as root inside the build container,
# from the directory build.sh pushed it into with the files beside it.
#
# Each step is one function, run in order at the bottom. A step that fails
# fails the build.
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
export DEBIAN_FRONTEND=noninteractive

# Rust lives in the image rather than in the session state, so every session
# starts with the same toolchain without downloading it.
export RUSTUP_HOME=/usr/local/rustup
export CARGO_HOME=/usr/local/cargo
export PATH="/usr/local/bun/bin:$CARGO_HOME/bin:$PATH"

# jj's release asset name carries the tag, so it is pinned here.
jj_version=v0.46.0

# Prints what is at an HTTPS URL, and fails on any error rather than printing
# a half.
get() {
    curl --proto '=https' --tlsv1.2 -fsSL "$1"
}

# Runs the shell installer at $1 with the interpreter $2 and the arguments
# after, from a file, so a download cut short is never half run.
run_installer() {
    url=$1 shell=$2
    shift 2
    get "$url" > /tmp/installer
    "$shell" /tmp/installer "$@"
    rm -f /tmp/installer
}

install_packages() {
    apt-get update -qq
    sed -e 's/#.*//' "$here/packages" |
        xargs apt-get install -y -qq --no-install-recommends
    apt-get clean
}

make_agent_account() {
    groupadd --gid 1000 agent 2>/dev/null || true
    useradd --uid 1000 --gid 1000 --home-dir /state/home --shell /bin/bash \
        --no-create-home agent 2>/dev/null || true
    install -d -o 1000 -g 1000 -m 0755 /state/home /workspace
    install -m 0440 "$here/sudoers" /etc/sudoers.d/agent
    visudo -cf /etc/sudoers.d/agent
    # Rootless podman inside the guest must stay inside the outer container
    # map, which covers guest ids 0..65535. 1000 is the agent, so its subids
    # start at 1001 rather than at the host default of 100000.
    grep -q '^agent:' /etc/subuid || echo 'agent:1001:64535' >> /etc/subuid
    grep -q '^agent:' /etc/subgid || echo 'agent:1001:64535' >> /etc/subgid
}

install_errand_files() {
    install -m 0755 "$here/errand-exec" "$here/errand-gui" "$here/kage" \
        /usr/local/bin/
    install -D -m 0644 "$here/docker-socket.conf" \
        /etc/systemd/system/docker.socket.d/errand.conf
    install -D -m 0644 "$here/containers.conf" /etc/containers/containers.conf
    install -m 0644 "$here/errand-kvm.service" /etc/systemd/system/
    systemctl enable docker.socket docker.service errand-kvm.service
}

# Debian names some tools differently, or keeps them off the default PATH.
link_usual_names() {
    ln -sf /usr/bin/fdfind /usr/local/bin/fd
    command -v go >/dev/null || ln -sf /usr/lib/go/bin/go /usr/local/bin/go
    command -v gofmt >/dev/null || ln -sf /usr/lib/go/bin/gofmt /usr/local/bin/gofmt
}

install_rust() {
    run_installer https://sh.rustup.rs sh -y --no-modify-path \
        --profile minimal --default-toolchain stable --component clippy,rustfmt
    # cross uses the guest's rootless podman, which errand-exec selects.
    cargo install cross --locked
    rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git"
    # The agent owns the toolchain, so it can add a toolchain, a target or a
    # component without sudo. What it adds lasts until its sandbox stops,
    # like anything else it installs outside its home.
    chown -R 1000:1000 "$RUSTUP_HOME" "$CARGO_HOME"
}

install_uv() {
    UV_UNMANAGED_INSTALL=/usr/local/bin run_installer https://astral.sh/uv/install.sh sh
}

install_zig() {
    index=$(get https://ziglang.org/download/index.json)
    version=$(printf '%s' "$index" |
        jq -r 'to_entries | map(select(.key != "master")) | .[0].key')
    url=$(printf '%s' "$index" | jq -r --arg v "$version" '.[$v]["x86_64-linux"].tarball')
    mkdir -p /usr/local/zig
    get "$url" | tar -xJ -C /usr/local/zig --strip-components=1
    chmod -R a+rX /usr/local/zig
    ln -sf /usr/local/zig/zig /usr/local/bin/zig
}

# The musl build is static, so it does not depend on the guest's libc.
install_jj() {
    get "https://github.com/jj-vcs/jj/releases/download/$jj_version/jj-$jj_version-x86_64-unknown-linux-musl.tar.gz" |
        tar -xz -C /usr/local/bin ./jj
}

install_bun() {
    BUN_INSTALL=/usr/local/bun run_installer https://bun.sh/install bash
    chmod -R a+rX /usr/local/bun
}

# Every tool the image promises answers, or the build fails here rather than
# in a session.
check_tools() {
    for tool in cargo rustc cross uv uvx bun jj kage node python3 gh; do
        "$tool" --version >/dev/null
    done
    go version >/dev/null
    zig version >/dev/null
}

# A published image is cloned, so each guest must make its own machine id
# rather than share one.
scrub() {
    : > /etc/machine-id
    rm -f /var/lib/dbus/machine-id
    rm -rf /tmp/* /var/tmp/*
}

install_packages
make_agent_account
install_errand_files
link_usual_names
install_rust
install_uv
install_zig
install_jj
install_bun
check_tools
scrub
