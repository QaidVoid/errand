---
name: errand-setup
description: >-
  Set up the errand daemon from scratch: survey the host, build the binary,
  write the configuration, run it, and verify the first session. Use when the
  user wants to install errand, configure errand, get errand serving a channel,
  or asks why errand will not start.
license: MIT
metadata:
  repo: https://github.com/QaidVoid/errand
---

# Errand setup

Take a host from nothing to a running errand daemon that serves one chat
channel. Work the steps in order. Each step says how to confirm it before
moving on.

## 0. Survey the host

Run the checker from the repository root. It only reads.

```sh
bash skills/errand-setup/scripts/check-host.sh
```

It reports the kernel, the sandbox backend, the agent, and the build tools.
Fix anything marked `missing` before continuing. There is no unsandboxed
mode, so a host with neither bailey nor rootless podman stops here: install
one of them first. See
[references/sandbox-backends.md](references/sandbox-backends.md) for how to
choose.

## 1. Collect the three things no default can supply

1. A chat bot token, the ID of the one channel to serve, and the account IDs
   that may drive sessions.
2. A credential for a model provider.
3. Absolute paths for `projectRoot` (where session projects live) and
   `stateDir` (where the daemon keeps its state). Both must be absolute.
   Relative paths are refused.

## 2. Build it

The web interface must be built before the binary, because `build.rs` embeds
`dist/web` into the binary at compile time. A binary built without it serves
no interface.

```sh
cd web && bun install && bun run build
cd .. && cargo build --release
```

Confirm: `target/release/errand` exists and is executable. That one file is
the whole daemon, interface and runtime included. A host that runs it needs
neither a checkout nor a toolchain.

## 3. Write the configuration

Copy `config.example.json` and edit the copy. Never edit the example in
place. The daemon reads the first file it finds in this order:

1. The path in `ERRAND_CONFIG`, which skips the search entirely.
2. `~/.config/errand/config.json`
3. `/etc/errand/config.json`
4. `config.json` in the working directory

The smallest file that starts:

```json
{
  "chat": {
    "token": "the bot token",
    "channelId": "the one channel to serve",
    "allowedUserIds": ["accounts that may drive sessions"]
  },
  "agent": {
    "provider": "anthropic",
    "providers": {
      "anthropic": {
        "credentialName": "ANTHROPIC_API_KEY",
        "credential": "the provider key"
      }
    }
  },
  "projectRoot": "/srv/errand/projects",
  "stateDir": "/var/lib/errand"
}
```

Then lock it down. It holds the bot token and the provider key.

```sh
chmod 0600 ~/.config/errand/config.json
```

Confirm: `config.schema.json` beside the example gives editors completion
and checking. Every other field has a documented default in
`docs/reference/configuration.md`. When unsure about a field, read that
page rather than guessing.

## 4. Run it

```sh
errand run
```

Read the startup report before touching the chat service. It names the
backend and states what it can and cannot enforce on this host. A gap stops
the daemon unless `sandbox.requireFullEnforcement` is set to `false`, and
running anyway means accepting the weaker boundary it stated.

Confirm it serves: post in the served channel. The first message opens a
thread and starts a session. Replies in that thread are prompts to the
agent. `errand threads` lists what past sessions left on disk.

## 5. When it will not start

The exit code names the kind of problem, so read it rather than retrying
blindly.

| code | meaning | what to do |
| ---- | ------- | ---------- |
| 2 | the configuration, the sandbox backend, or the token was refused | read the log line, fix the named field, run again |
| 3 | the backend cannot enforce a guarantee the configuration demands | see the startup gap report, then change the host or set `sandbox.requireFullEnforcement` to `false` on purpose |
| 4 | another daemon already holds this state directory | do not start a second one; two daemons on one bot token both act on every message. A lock left by a dead process is taken over on its own |

## 6. Change it while it runs

The daemon re-reads the configuration file when it changes, so most edits
take effect without a restart and apply to the next session. Three things
stay fixed until a restart: the connection and the chat token, the sandbox
backend with the egress broker and provider routes, and sessions already
running. A file that cannot be read is refused whole and logged with every
reason while the daemon keeps serving under what it already had.

## Skill files

- `scripts/check-host.sh` is a read only survey of kernel, backend, agent,
  and build tools.
- `references/sandbox-backends.md` covers choosing between bailey and podman
  and what each one needs.
