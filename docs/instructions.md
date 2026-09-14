# Instructions

Two things tell a session how to work before anybody types a prompt. They come
from different people and they are set in different places.

## What a project asks for

The agent reads `AGENTS.md` itself. It looks in the working directory, in every
directory above it, and in its own configuration directory, and it uses
`CLAUDE.md` where there is no `AGENTS.md`. A directory holding
`AGENTS.override.md` has that read instead of either. Everything found is
concatenated.

Nothing has to be configured for this. A session's working directory is the
project directory, so a repository that carries an `AGENTS.md` is already
understood by every session that works in it.

That file belongs to the repository, which is the point of it: the conventions
of a project should arrive with the project rather than being remembered by
whoever is driving.

## What the operator asks for

A repository cannot say what you want of every session, and most repositories
say nothing at all. `agent.rulesPath` names a file of standing instructions that
every session is given, whatever it is working on:

```json
{
  "agent": {
    "rulesPath": "/etc/errand/AGENTS.md"
  }
}
```

Setting it is usually unnecessary. When no path is named, errand looks for
`AGENTS.md` beside the configuration file it actually loaded, so a file at
`~/.config/errand/AGENTS.md` is picked up on its own. The default follows the
configuration in force rather than a fixed location, so somebody with a file in
their home directory and another in `/etc` gets the one belonging to the
configuration being used.

A path you name is absolute, because the daemon's working directory is not yours
and a relative path would name a different file depending on where it was
started.

The file is read on the host, not inside the sandbox, so it needs no grant under
`sandbox.policyExtra` and a session never sees the file itself. Only the text
reaches the agent.

It is re-read for each session. Editing it changes what the next session is
told, with no restart, and emptying it means there are no house rules rather
than an empty heading.

**Named but unreadable stops the daemon.** An operator who has configured rules
believes every session carries them, and a mistyped path that only ever showed
up as a line in a log would leave that belief standing while no session got
them. The exit code is 2, the same as any other refused configuration.

The default is held to a different standard. No `AGENTS.md` beside the
configuration simply means no house rules, because a default nobody asked for
must not be able to stop the daemon.

## When the two disagree

The agent is told which is which, and asked to say so rather than to choose one
in silence. A project that forbids what the house requires is a decision for a
person, and the useful behaviour is to surface it rather than to resolve it.

## What this costs

Both are paid for in the agent's context on every turn of every session. That is
the reason to keep house rules to what actually matters: a paragraph that
applies everywhere is worth its place, and a page of preferences is bought again
on every message anybody sends.

Facts the agent learns as it goes are a separate thing, with their own bounds.
See [memory](/memory).
