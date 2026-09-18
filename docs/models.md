# Two models

A session runs on one model, and there are two ways to bring a cheaper one into
the work. They answer different questions, and only one of them lets the cheap
model do anything.

## Switching the model a session runs on

`!model` lists what this host knows the provider serves. `!model <name>` moves
the session to that one, **keeping the conversation**: what was said stays said,
and the next turn is answered by the model named.

That is the useful shape for "plan on the capable model, carry it out on the
cheap one". The cheap model works in the same thread, with the same tools, in
the same sandbox, and you switch back to review. Switching is refused while a
turn is running, because changing the model underneath a turn answers half a
question with each.

## How hard a model thinks

A thinking level is written onto a model name with a colon, as
`musecringe:max`, and typing one is always the last word. Saying it on every
switch is tedious, so a level can be settled once in the configuration:

```json
{
  "agent": {
    "providers": {
      "ajamxhacker": {
        "credential": "...",
        "defaultThinkingLevel": "high",
        "models": [{ "id": "musecringe", "defaultThinkingLevel": "max" }]
      }
    }
  }
}
```

The model's own level wins over the provider's, and the provider's applies to
every model it serves. `!model` says which level a model will think at when
nobody asks, so the list is enough to know what a switch will do.

Naming a model the host's store already lists does not add a second copy of it.
It says something about the one that is there, which is how a level is set for
a model of the provider the session starts on.

## Asking a cheaper model about one thing

A delegation is a question about one artefact that already exists. The agent
runs a command:

```sh
delegate --file src/parse.ts "which functions does this export?"
delegate --call <tool call id> "what failed, and on which line?"
delegate --attachment screenshot.png "transcribe the error"
```

The daemon reads what was named, sends it with the question, and hands the
answer back labelled with the model that produced it.

Turn it on by naming a model:

```json
{
  "agent": {
    "delegate": { "model": "glm-5.3-flash", "perTurn": 8, "deadlineMs": 60000 }
  }
}
```

Absent means no delegation at all, and the agent is not told about a command it
does not have.

### What the cheap model can do

Nothing. It is sent one message holding the question and the artefact, with no
tools, so it cannot read another file, run a command, or change anything. It is
not told what the session is trying to achieve. What comes back is text.

This is what makes it safe to ask freely: a question phrased as a decision
produces an opinion about an artefact, which is as harmless as a summary of one,
and the answer can always be checked against the same artefact.

### What it is for

Keeping material out of the session's own context. A log read into the
conversation is paid for again on every later turn, because each turn replays
what came before. Asking about it instead costs one cheap request and returns a
paragraph.

`!status` reports what a session has delegated: how many were asked, what they
cost, and how much was kept out of the conversation.

### When it does not work

A refusal is ordinary and never fails a turn: the work stays with the session's
own model, and the thread says so once. A delegation is refused when it names
nothing, names more than one thing, names a path outside the project, when the
turn has used its allowance, or when the provider is being backed off.

## What is not here

There is no automatic handoff: nothing writes a plan and hands it to a second
agent to implement unattended. That needs a second agent with its own sandbox,
its own turn loop, and a way to review and stop it, and the failure mode is the
expensive one, a cheap model producing plausible wrong work that the capable one
then has to diagnose and redo.
