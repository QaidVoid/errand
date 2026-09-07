# Getting started

## What you need

- A chat bot token, and one channel for it to serve.
- A credential for a model provider.
- Linux, with either [bailey](https://github.com/qaidvoid/bailey) or rootless
  podman. The sandbox is not optional, so one of them has to be there.

## Build it

```sh
git clone https://github.com/QaidVoid/errand
cd errand
deno task build
```

That produces `dist/errand`: one binary carrying the web interface and its own
runtime, with what it may do compiled into it. A host that runs it needs neither
a checkout nor deno.

## Configure it

Write `~/.config/errand/config.json`. The smallest file that starts:

```json
{
  "chat": {
    "token": "the bot token",
    "channelId": "the one channel to serve",
    "allowedUserIds": ["the accounts that may drive sessions"]
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

Everything else has a default, listed in [configuration](/reference/configuration).
Keep the file mode 0600: it holds two credentials.

## Run it

```sh
errand run
```

It checks the sandbox before it touches the chat service, and says what it can
and cannot enforce on this host. If it cannot enforce something the
configuration asks for, it stops rather than starting anyway. See
[sandboxing](/sandboxing) for what that report means.

Then post in the served channel. The first message opens a thread and starts a
session; replies in that thread are prompts.

## When it will not start

The exit code says which kind of problem it was, so a service can tell a
refusal from a crash.

| code | what happened                                                    |
| ---- | ---------------------------------------------------------------- |
| 2    | the configuration, the sandbox backend, or the token was refused |
| 3    | the backend cannot enforce a guarantee the configuration demands |
| 4    | another daemon already holds this state directory                |

Two daemons on one bot token both act on every message, which is what 4
prevents. A lock left by a process that is gone is taken over.
