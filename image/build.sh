#!/bin/sh
# Builds the errand guest image with Incus, on the host the backend runs on.
#
# A temporary container is provisioned by provision.sh, then published under
# the alias and, with INCUS_GOLDEN, copied to the golden instance the backend
# starts sessions from. Publishing over the alias deletes the image it named
# before.
set -eu

: "${KAGE_BIN:?set KAGE_BIN to a kage binary newer than 0.5.1}"
project=${INCUS_PROJECT:-errand}
alias=${INCUS_ALIAS:-errand-agent}
golden=${INCUS_GOLDEN:-}
build=errand-image-build
stage=/root/errand-image
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

# What provision.sh reads beside itself, from this directory.
staged="provision.sh packages sudoers errand-exec errand-gui errand-kvm.service
docker-socket.conf containers.conf"

# The project goes before the subcommand: after an exec's `--` it would be
# an argument to the guest's command instead.
incus_() {
    incus --project "$project" "$@" </dev/null
}

# A golden instance carries the contract. Rebuilding only the image would
# leave the backend starting sessions from the golden as it was.
refuse_forgotten_golden() {
    [ -z "$golden" ] || return 0
    for instance in $(incus_ list --format csv --columns n); do
        if [ -n "$(incus_ config get "$instance" user.errand.contract)" ]; then
            echo "$instance is a golden instance, so build with INCUS_GOLDEN=$instance" >&2
            exit 1
        fi
    done
}

remove_build() {
    incus_ delete --force "$build" >/dev/null 2>&1 || true
}

provision() {
    incus_ launch images:debian/13 "$build"
    for file in $staged; do
        incus_ file push "$here/$file" "$build$stage/$file" --create-dirs
    done
    incus_ file push "$KAGE_BIN" "$build$stage/kage"
    incus_ exec "$build" -- sh "$stage/provision.sh"
    incus_ exec "$build" -- rm -rf "$stage"
}

# The contract tells the backend what the guest provides before it starts.
# kage's own provider list is read from the kage just installed, so the two
# cannot drift.
contract() {
    providers=$(incus_ exec "$build" -- kage providers --json)
    printf '{"version":1,"guestUid":1000,"dockerGid":1000,"kageProviders":%s}' "$providers"
}

# Given to publish rather than set afterwards, so the contract is written into
# the image's own metadata and survives `incus image export`.
publish() {
    incus_ publish "$build" --alias "$alias" --reuse "user.errand.contract=$1"
}

# Copied aside first and renamed into place, so the golden is missing for a
# moment rather than for the length of a copy.
replace_golden() {
    incus_ delete --force "$golden-next" >/dev/null 2>&1 || true
    incus_ copy "$build" "$golden-next"
    incus_ config set "$golden-next" user.errand.contract "$1"
    incus_ delete --force "$golden" >/dev/null 2>&1 || true
    incus_ move "$golden-next" "$golden"
}

refuse_forgotten_golden
trap remove_build EXIT
remove_build
provision
contract=$(contract)
incus_ stop "$build"
publish "$contract"
echo "published $alias in project $project"
if [ -n "$golden" ]; then
    replace_golden "$contract"
    echo "replaced the golden instance $golden"
fi
