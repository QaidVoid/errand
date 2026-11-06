# Memory

A session's sandbox goes away when it ends, but what the agent learned does not.
errand keeps two kinds of short facts between conversations.

| Kind | Follows | Good for |
| --- | --- | --- |
| **About a person** | Whoever is speaking, into any thread they open | Preferences, how they like things done, what they are working on |
| **About a project** | The working directory, whoever starts the next thread | Conventions, approaches already rejected, where things live |

## How a fact is stored

The sandbox cannot call back into the daemon, so remembering is a file write.
The agent appends a line to one of two files in its state directory:

- `remember.md` for a fact about the person
- `project-notes.md` for a fact about the project

After each turn the daemon reads both files, stores what it finds, and empties
them. Storing is idempotent, so a fact offered twice is kept once.

## What the agent is told

Before each turn the stored facts are added to the agent's system prompt. The
same block is written to `memory.md` in the session's state directory, so you
can read exactly what the agent was told.

| Limit | Value |
| --- | --- |
| One fact | 300 characters |
| The whole block | 2000 characters, newest facts first |
| Facts per project | 500 |

Newest first matters: when two facts disagree, the later one is usually the
correction, so it is the one that fits the budget.

## Reading and forgetting

| Command | Does |
| --- | --- |
| `!facts` | Show what is remembered about you. |
| `!facts @somebody` | Show what is remembered about somebody else. |
| `!facts project` | Show what is remembered about this project. |
| `!forget @somebody` or `!forget project` | Drop those facts. |

Everything lives in one SQLite file, `<stateDir>/memory.db`. Nothing in it is
encrypted, and you can open it directly.

::: warning Memory is not a safe place for secrets
The agent is told never to record credentials or anything said in confidence,
but that is an instruction, not a control. A line it writes is stored like any
other. Treat memory as readable by anyone who can start a session as that
person, and keep secrets out of the conversation.
:::
