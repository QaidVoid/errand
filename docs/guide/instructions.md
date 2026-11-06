# Instructions

Two sets of standing instructions reach a session before anybody types a
prompt. One comes from the project, the other from you as the operator.

## Project instructions

The agent reads `AGENTS.md` on its own. It looks in the working directory, in
every directory above it, and in its own configuration directory. It falls back
to `CLAUDE.md` where there is no `AGENTS.md`, and a directory holding
`AGENTS.override.md` has that read instead of either. Everything it finds is
joined together.

Nothing needs configuring. A session works inside its project directory, so a
repository that carries an `AGENTS.md` brings its conventions with it.

## House rules

`agent.rulesPath` names a file of instructions every session gets, whatever it
is working on:

```json
{ "agent": { "rulesPath": "/etc/errand/AGENTS.md" } }
```

You usually do not need to set it. Without it, errand looks for `AGENTS.md`
next to the configuration file it loaded, so `~/.config/errand/AGENTS.md` is
picked up automatically.

- **The path must be absolute.** The daemon's working directory is not yours.
- **It is read on the host.** The sandbox never sees the file, only its text,
  so it needs no `policyExtra` grant.
- **It is re-read for every session.** Edits apply to the next session without
  a restart. An empty file means no house rules.

::: warning A named file that cannot be read stops the daemon
If you set `rulesPath` and the file is missing or unreadable, errand exits with
code 2 instead of running sessions without the rules you expect. A missing
default `AGENTS.md` next to the configuration is not an error. It just means
there are no house rules.
:::

## When the two disagree

The agent is told which instructions came from the project and which from the
house, and is asked to point out a conflict rather than silently pick a side.

## Keep them short

Both sets are sent on every turn of every session, so they cost context every
time. Keep house rules to what applies everywhere, and put project specifics in
the project's own `AGENTS.md`.

## Plugins

kage plugins are another way to shape every session. List them under
`agent.plugins`, each with the capabilities it is granted:

```json
{
  "agent": {
    "plugins": [{ "path": "/etc/errand/plugins/review.lua", "capabilities": ["net"] }]
  }
}
```

The file is copied into the agent's plugin directory when a session launches.
Capabilities are never guessed: a plugin missing one it needs disables itself
when kage loads it.

Facts the agent learns as it works are kept separately. See
[memory](/guide/memory).
