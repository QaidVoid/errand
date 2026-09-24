# Two models

A session runs on one model, and there are two ways to bring a cheaper one into
the work. They answer different questions, and only one of them lets the cheap
model do anything.

## Naming a model

A model is named the same way everywhere: in `agent.model`, after `--model` in
the message that starts a session, and after `!model` in a thread.

```
musecringe                  a model of the provider the session is on
ajamxhacker/musecringe      a model of another provider you have defined
ajamxhacker/musecringe:max  the same, thinking as hard as it can
muse                        whatever `aliases` says that stands for
```

Naming a provider settles which one the session runs on, so it beats the
standing `agent.provider`. A leading segment counts as a provider only when it
is one your configuration defines, which leaves a model id holding a slash of
its own, `meta/muse-spark-1.3`, whole.

A provider the agent has built in needs only its `credential` to be switched
to. The daemon reads where it is served from the host's model store, so it
does not have to be the provider a session started on.

## Asking a provider for its models

A provider can be asked which models it serves instead of having them listed
by hand. Set `discover` on it, and errand reads the listing under its `baseUrl`
when the daemon starts:

```json
{
  "agent": {
    "providers": {
      "gateway": {
        "baseUrl": "https://gateway.example/v1",
        "api": "openai-completions",
        "credential": "...",
        "discover": { "defaults": { "reasoning": true } },
        "models": [{ "id": "glm-5.3-flash", "contextWindow": 256000 }]
      }
    }
  }
}
```

`true` reads the usual `/models` listing, with the sizes under
`context_window` and `max_output_tokens`. A provider shaped otherwise says
where things are:

```json
"discover": {
  "path": "/api/models",
  "list": "result.models",
  "fields": { "id": "slug", "contextWindow": "top_provider.context_length" }
}
```

What is found goes underneath what `models` says. An entry for a model that
was found overrides only the fields it names, so the example above keeps the
listed `maxTokens` and replaces the window. `defaults` fills fields no listing
carries, such as `reasoning`, for every model found. A provider that cannot be
asked keeps the models it names, and the log says why.

An operator can ask again without a restart with `!models refresh`. Every
session's `!model` sees the new list at once. A running sandbox keeps what it
was launched with until its next launch.

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
