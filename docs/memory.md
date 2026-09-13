# Memory

A session ends and its sandbox goes. What the agent learned about you, and about
the project, does not. Two kinds of fact are carried between conversations, and
both are short lines of plain text rather than a transcript.

**About a person.** These follow whoever is speaking, into whatever thread they
open next. Preferences, how somebody wants things done, what they are working
on.

**About a project.** These follow the working directory instead, so a decision
taken in one thread is known to the next thread that works there, whoever starts
it. Conventions, approaches already tried and rejected, where things live.

The split is the point. A preference belongs to a person and travels with them
between projects. A convention belongs to a repository and applies to everyone
who works in it.

## How a fact gets stored

The agent cannot call back into the daemon. That is the sandbox working as
intended, and it means remembering something has to be a file write rather than
a request.

So the agent appends a line to one of two files in its own state directory:
`remember.md` for a person, `project-notes.md` for the project. After each turn
the daemon reads both, stores what it finds, and empties them. Emptying rather
than deleting is deliberate: the agent's next append lands in a file it already
knows exists, and no line is ever ingested twice.

Storing is idempotent. The same fact offered twice is stored once, so an agent
that repeats itself does not fill your context with duplicates.

## Where it lives

One SQLite database, at `<stateDir>/memory.db`. It holds kilobytes of short
lines, which is why it is a file the runtime can already open rather than a
service to run.

You can read it. Nothing in it is encrypted and nothing is hidden from the
operator, because it is your machine and these are facts about your project and
the people using it.

## What reaches the model

Before a turn, the facts are rendered into a block and appended to the agent's
system prompt. The block is written to `memory.md` in the session's state
directory, so what the agent was told is on disk and can be read back.

Three bounds apply, and they exist because every remembered line is paid for in
the agent's context on every session that person starts:

| bound         | value           | why                                                                                                      |
| ------------- | --------------- | -------------------------------------------------------------------------------------------------------- |
| one fact      | 300 characters  | a fact that needs a paragraph is a document, and belongs in the repository                               |
| the block     | 2000 characters | newest facts win, because a contradiction is usually a correction                                        |
| project facts | 500             | a working directory that has accumulated more than this has stopped being memory and started being a log |

Newest first is worth understanding. When two facts disagree, the later one is
almost always the correction, so it is the one that survives the budget.

## What this is not

**The instruction not to record a secret is an instruction, not a boundary.**
The agent is told, in the same block, never to write down credentials or
anything it was told in confidence. A model that ignores that has written a line
to a file, and the daemon will store it like any other. Treat memory as readable
by anyone who can start a session as that user, and keep secrets out of the
conversation rather than relying on the agent to keep them out of the file.

This is the same honesty the rest of this documentation tries to keep. A
mitigation described as a guarantee is worse than no mitigation, because it
takes away the chance to decide about it.

## Known gap

There is no way to ask, from a thread, to be forgotten. The store can drop
everything held for one person or one project, and nothing calls it. Until
something does, forgetting is done by editing `memory.db` directly.
