# Delegation

Delegation lets the session's model ask a cheaper model one question about one
thing that already exists: a file, a tool call's output, or an attachment. The
answer comes back as text. The material itself never enters the session's
context.

## Why

Everything in a conversation is paid for again on every later turn, because
each turn replays what came before. Reading a long log into the conversation
costs that log on every turn after. Asking a cheap model about it instead costs
one small request and returns a paragraph.

## Turning it on

Name the model to delegate to:

```json
{
  "agent": {
    "delegate": { "model": "glm-5.3-flash", "perTurn": 8, "deadlineMs": 60000 }
  }
}
```

| Field | Meaning |
| --- | --- |
| `model` | The model asked. It must be served by the same provider, under the same key. |
| `perTurn` | How many delegations one turn may make. |
| `deadlineMs` | How long one delegation may take before it is abandoned. |

Without `agent.delegate` there is no delegation, and the agent is not told
about the command.

## How the agent uses it

The agent runs a `delegate` command, naming exactly one thing:

```sh
delegate --file src/parse.ts "which functions does this export?"
delegate --call <tool call id> "what failed, and on which line?"
delegate --attachment screenshot.png "transcribe the error"
```

The daemon reads what was named, sends it with the question, and returns the
answer labelled with the model that wrote it.

## What the cheap model can do

Nothing beyond answering. It receives one message holding the question and the
material, with no tools. It cannot read another file, run a command, or change
anything, and it is not told what the session is working on. That is what makes
delegation safe to use freely: the worst it can produce is a wrong description,
and the answer can always be checked against the same material.

## When a delegation is refused

A refused delegation never fails the turn. The session's own model carries on,
and the thread says so once. A delegation is refused when it:

- names nothing, or more than one thing
- names a path outside the project
- would go over the turn's `perTurn` allowance
- targets a provider that is being backed off

`!status` reports how many delegations a session made, what they cost, and how
much material they kept out of the conversation.

## Delegation is not a handoff

There is no mode where one model writes a plan and a second agent implements it
unattended. To have a cheaper model do the work rather than describe it, switch
the session with [`!model`](/guide/models#switching-models) and switch back to
review.
