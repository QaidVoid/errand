# Quick start

This page takes you from a clone to a first session.

## Requirements

| You need | Why |
| --- | --- |
| A Discord bot token and one channel for it to serve | Sessions start from messages in that channel. |
| A credential for a model provider | The agent needs a model to talk to. |
| Linux with [bailey](https://github.com/QaidVoid/bailey), rootless podman, or Incus | The sandbox is not optional. `sandbox.backend` picks one and defaults to `bailey`. |
| [kage](https://github.com/QaidVoid/kage) newer than 0.5.1 | The agent. On the host's PATH under bailey, or inside the image under podman and Incus. |
| A Rust toolchain and [bun](https://bun.sh) | To build the binary and the web interface it embeds. |

errand asks kage which providers it knows (`kage providers --json`) at startup
and refuses to start when it cannot answer. The Incus backend needs some host
setup first, described in [the Incus backend](/operations/incus).

## Build

```sh
git clone https://github.com/QaidVoid/errand
cd errand

# Build the interface first. The binary embeds it at compile time.
(cd web && bun install && bun run build)

cargo build --release
install -Dm 0755 target/release/errand ~/.local/bin/errand
```

The result is one self-contained binary at `target/release/errand`. The host
that runs it needs neither the checkout nor a toolchain.

## Configure

Create `~/.config/errand/config.json`. This is the smallest file that starts:

```json
{
  "chat": {
    "token": "YOUR_BOT_TOKEN",
    "channelId": "THE_CHANNEL_TO_SERVE",
    "allowedUserIds": ["ACCOUNTS_THAT_MAY_START_SESSIONS"]
  },
  "agent": {
    "provider": "anthropic",
    "providers": {
      "anthropic": {
        "credentialName": "ANTHROPIC_API_KEY",
        "credential": "YOUR_PROVIDER_KEY"
      }
    }
  },
  "projectRoot": "/srv/errand/projects",
  "stateDir": "/var/lib/errand"
}
```

The file holds two credentials, so keep it at mode `0600`. Every other field
has a default, listed in the [configuration reference](/reference/configuration).
The repository also has `config.example.json` to copy, and a JSON schema that
gives your editor completion and checking.

::: tip Where the file is read from
`~/.config/errand/config.json`, then `/etc/errand/config.json`, then
`config.json` in the working directory. Set `ERRAND_CONFIG` to name one
directly and skip the search.
:::

## Run

```sh
errand run
```

Before it connects to the chat service, errand probes the sandbox backend and
prints what it can and cannot enforce on this host:

```
sandbox backend: bailey
  sessions run as confined host processes using the host's own tools
  no single file may exceed 1g, enforced as an rlimit
  1 guarantee(s) cannot be enforced on this host:
    - per-session memory, cpu, and process limits are not applied: this host
      reports no cgroup delegation
```

By default any gap stops the daemon. Read what is missing, then either fix the
host or set `sandbox.requireFullEnforcement` to `false` to run anyway. The
[sandboxing](/operations/sandboxing#the-startup-report) page explains each line.

## Start a session

Post in the served channel:

```
fix the failing test in src/parse.rs
```

errand opens a thread named after the message and starts a session there.
Reply in the thread to keep going. Type `!help` to see every command.

## Next steps

- [Run it as a service](/operations/service) under systemd or OpenRC.
- [Narrow network egress](/operations/egress) to the hosts your projects need.
- [Connect GitHub or Forgejo](/guide/forges) so issues can start sessions.
