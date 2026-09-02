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

## Status

Early. The daemon is being built module by module, and this README grows with
it.

## Development

```sh
deno task check   # formatting, lint, types, tests, and the ASCII rule
deno task test    # the test suite
deno task start   # run the daemon against ./config.json
```

The daemon runs under an explicit permission set rather than with the whole
machine available to it, which is visible in `deno task start`.

Project rules are in [AGENTS.md](AGENTS.md).
