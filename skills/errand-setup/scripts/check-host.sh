#!/usr/bin/env bash
# Survey a host for errand prerequisites. Read only: it checks for tools and
# reports, and changes nothing.
#
# Usage: bash skills/errand-setup/scripts/check-host.sh
#
# Exits 0 when a sandbox backend is present, 1 when neither bailey nor
# rootless podman is found, since without one the daemon refuses to start.

set -u

ok=0
missing=0
backend=""

report() {
    if [ "$1" = ok ]; then
        printf 'ok: %s\n' "$2"
        ok=$((ok + 1))
    else
        printf 'missing: %s\n' "$2"
        missing=$((missing + 1))
    fi
}

if [ "$(uname -s)" = Linux ]; then
    report ok "Linux kernel $(uname -r)"
else
    report missing "Linux is required, found $(uname -s)"
fi

if command -v bailey >/dev/null 2>&1; then
    backend="bailey"
    report ok "bailey sandbox backend at $(command -v bailey)"
elif command -v podman >/dev/null 2>&1; then
    backend="podman"
    report ok "podman sandbox backend at $(command -v podman)"
else
    report missing "a sandbox backend: install bailey or rootless podman"
fi

if [ "$backend" = bailey ]; then
    if command -v pi >/dev/null 2>&1; then
        report ok "pi agent at $(command -v pi)"
    else
        report missing "the pi agent on PATH: the bailey backend runs the host install"
    fi
    if bailey doctor >/dev/null 2>&1; then
        printf '%s\n' '--- bailey doctor ---'
        bailey doctor 2>&1 | head -n 20
    else
        report missing "a working bailey doctor run"
    fi
fi

if [ "$backend" = podman ]; then
    if podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null | grep -q true; then
        report ok "podman runs rootless"
    else
        report missing "rootless podman: this backend requires it"
    fi
fi

for tool in cargo bun; do
    if command -v "$tool" >/dev/null 2>&1; then
        report ok "build tool $tool at $(command -v "$tool")"
    else
        report missing "build tool $tool, needed to build errand from source"
    fi
done

printf 'result: %d ok, %d missing\n' "$ok" "$missing"

if [ -n "$backend" ]; then
    exit 0
else
    exit 1
fi
