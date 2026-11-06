# errand

Run a coding agent from a chat channel, in a sandbox it cannot escape.

A message in one Discord channel opens a thread and starts a session. The agent
works in a project directory of its own and nowhere else. Replies in the thread
are prompts, and what the agent says, runs, and changes comes back to the same
thread.

- One channel, one thread per session, several sessions at once.
- Every session is sandboxed under bailey, rootless podman, or Incus. There is
  no unsandboxed mode.
- A per-session egress broker holds the provider key, so the sandbox only ever
  sees a stand-in, and limits which hosts a session reaches.
- Switch models mid-conversation, start sessions from GitHub or Forgejo issues,
  and follow everything in an optional web interface.

The full documentation is in [`docs/`](docs).

## Requirements

- Linux with [bailey](https://github.com/QaidVoid/bailey), rootless podman, or
  Incus. `sandbox.backend` picks one and defaults to `bailey`.
- [kage](https://github.com/QaidVoid/kage) newer than 0.5.1, the agent that
  runs inside each session. It must be on the host's PATH under bailey, or
  inside the image under podman and Incus.
- A Discord bot token and one channel for it to serve.
- A credential for a model provider.
- A Rust toolchain and [bun](https://bun.sh) to build.

## Quick start

```sh
git clone https://github.com/QaidVoid/errand
cd errand
(cd web && bun install && bun run build)   # the interface the binary embeds
cargo build --release
install -Dm 0755 target/release/errand ~/.local/bin/errand
```

Write `~/.config/errand/config.json`:

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

Then run it and post in the channel:

```sh
errand run       # run the daemon until it is told to stop
errand threads   # list, inspect, remove and collect what sessions left behind
errand help
```

The daemon probes the sandbox before connecting, and refuses to start when the
host cannot enforce what the configuration asks for. See
[Quick start](docs/guide/quick-start.md) and
[Sandboxing](docs/operations/sandboxing.md).

[`config.example.json`](config.example.json) is a fuller file to copy, and
`config.schema.json` gives an editor completion and checking. Service
definitions for systemd and OpenRC are in [`packaging/`](packaging).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- run
```

The reference pages under `docs/reference/` are generated from the code, and
`cargo test` fails when they drift. Regenerate them with
`cargo test regenerate_the_reference_pages -- --ignored`.

The documentation site is VitePress:

```sh
cd docs && bun install && bun run dev
```

Project rules are in [AGENTS.md](AGENTS.md).

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
