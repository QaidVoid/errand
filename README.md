# errand

Run a coding agent from a chat channel, in a sandbox it cannot escape.

A message in one configured channel opens a thread and starts a session. The
agent works in a project directory of your choosing and nowhere else. Replies
in the thread are prompts; what the agent says, runs, and changes comes back
to the same thread.

- One channel, one thread per session, several sessions at once.
- The agent runs sandboxed. There is no unsandboxed mode.
- A queue bounds how much work reaches the model provider at once, so a burst
  of messages cannot get an account rate limited.
- The chat token never enters a sandbox.

## Running it

```sh
errand run       # run the daemon until it is told to stop
errand threads   # list, inspect, and remove what past sessions left on disk
errand help
```

The configuration file is read from `~/.config/errand/config.json`, then
`/etc/errand/config.json`, then `config.json` in the working directory.
`ERRAND_CONFIG` names one outright and skips the search. A minimal one:

```json
{
  "chat": {
    "token": "the bot token",
    "channelId": "the one channel to serve",
    "allowedUserIds": ["accounts that may drive sessions"]
  },
  "agent": {
    "provider": "anthropic",
    "credentialName": "ANTHROPIC_API_KEY",
    "credential": "the provider key"
  },
  "projectRoot": "/srv/errand/projects",
  "stateDir": "/var/lib/errand"
}
```

Everything else has a documented default. The daemon refuses to start rather
than run with a guarantee it cannot keep: if the backend cannot enforce
everything configured on this host, it says which and stops, unless
`sandbox.requireFullEnforcement` is set to `false`.

## Two models, one session

A session can ask a cheaper model of the same provider one question about one
thing that already exists: a long log, a large file, a diff. It is shown that
one thing and nothing else, has no way to run or read anything, and answers in
text, so what comes back is a description to check rather than a decision to
follow. The point is what it keeps out of the session's own context, which is
paid for again on every later turn.

```json
{
  "agent": {
    "delegate": { "model": "glm-5.3-flash", "perTurn": 8, "deadlineMs": 60000 }
  }
}
```

Absent means no delegation at all. With it, the agent gets a `delegate` command
and is told how to use it; `!status` reports what it cost and how much it kept
out. To have the cheaper model do the work rather than describe it, use
`!model` instead.

## The interface

An optional local web interface reads a session as it happens, browses the
project, and starts new ones. It has no login: the address it binds to is the
access control, and a public bind is refused rather than warned about. Build it
once with `deno task build`, then add a `web` section to the configuration:

```json
{ "web": { "host": "127.0.0.1", "port": 8787 } }
```

Set `"observer": true` to serve one that can watch and read but change nothing.

## In a thread

`!help` lists what can be typed, `!usage` says how much of the provider's usage
window is left and when it resets, and `!model` moves the session to another
model of the same provider, keeping the conversation. Plan on the capable one,
switch to the cheap one to carry it out, switch back to review. The same commands are registered as slash
commands, so they can be picked rather than remembered. A message starting
`!!!` is an aside: the people in the thread see it and the agent is never told.

## Running it as a service

Definitions for OpenRC and systemd are in [packaging](packaging), along with
what to prepare and what its exit codes mean.

## Status

Early, but complete enough to run: chat, sandboxed sessions, the interface, and
service definitions for both init systems.

## Development

```sh
deno task check     # formatting, lint, types, tests, the ASCII rule, the interface
deno task test      # the test suite
deno task start     # run the daemon from the checkout
deno task build     # the interface and a single binary, into dist/
deno task dev:web   # the interface against a running daemon
```

`deno task build` produces `dist/errand`: one binary carrying the interface and
its own runtime, with what it may do compiled in, so a host that runs it needs
neither a checkout nor deno.

The interface keeps its dependencies in `web/deno.json` rather than the root
one. They are build tools, and sharing an import map put every one of them
inside the daemon's binary: about a hundred megabytes of bundler that never
runs at runtime.

The daemon runs under an explicit permission set rather than with the whole
machine available to it, which is visible in `deno task start`.

Project rules are in [AGENTS.md](AGENTS.md).

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
