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

## The interface

An optional local web interface reads a session as it happens, browses the
project, and starts new ones. It has no login: the address it binds to is the
access control, and a public bind is refused rather than warned about. Build it
once with `deno task build:web`, then add a `web` section to the configuration:

```json
{ "web": { "host": "127.0.0.1", "port": 8787 } }
```

Set `"observer": true` to serve one that can watch and read but change nothing.

## In a thread

`!help` lists what can be typed, and `!usage` says how much of the provider's
usage window is left and when it resets. The same commands are registered as slash
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
deno task start     # run the daemon against ./config.json
deno task build:web # build the interface into dist/web
deno task dev:web   # the interface against a running daemon
```

The daemon runs under an explicit permission set rather than with the whole
machine available to it, which is visible in `deno task start`.

Project rules are in [AGENTS.md](AGENTS.md).
