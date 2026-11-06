# Models

A session runs on one model at a time, and you can switch it mid-conversation.
This page covers how models are named, how providers are defined, and how
errand tracks usage windows.

## Naming a model

A model is named the same way everywhere: in `agent.model`, after `--model` (or
`-m`) in the message that starts a session, and after `!model` in a thread.

| Name | Means |
| --- | --- |
| `glm-5.3` | A model of the provider the session is on. |
| `acme/coder` | A model of another provider you defined. |
| `acme/coder:max` | The same model, thinking as hard as it can. |
| `fast` | Whatever `agent.aliases` says `fast` stands for. |

A leading segment is read as a provider only when your configuration defines a
provider by that name, so a model id with a slash of its own, such as
`meta/muse-spark-1.3`, stays whole.

A bare name is looked up on the session's own provider first, then on every
other provider you defined. When more than one of them serves it, errand
refuses and lists each `provider/model` that matches, rather than guessing.

```json
{
  "agent": {
    "model": "acme/coder:high",
    "aliases": { "fast": "acme/coder-mini", "big": "anthropic/claude-opus-5-5" }
  }
}
```

## Switching models

`!model` lists every model this host can run, as `provider/model`, with the
session's own provider first and the current model marked. Type one back to
switch:

```
!model acme/coder-mini
```

The switch **keeps the conversation**. The next turn is answered by the new
model with the same history, tools, and sandbox. That makes a useful pattern:
plan on a capable model, switch to a cheaper one to carry the plan out, then
switch back to review.

A switch requested while a turn is running waits for that turn to finish, and
the thread says it is waiting. A later switch replaces an earlier one that is
still waiting.

## Thinking levels

Add a level to a model name with a colon, as `coder:max`. The levels are `off`,
`minimal`, `low`, `medium`, `high`, `xhigh` and `max`. A level you type always
wins. To avoid typing one every time, set a default per provider or per model:

```json
{
  "agent": {
    "providers": {
      "acme": {
        "credential": "...",
        "defaultThinkingLevel": "high",
        "models": [{ "id": "coder", "defaultThinkingLevel": "max" }]
      }
    }
  }
}
```

The model's level beats the provider's. `!model` shows the level each model
will use when nobody asks for one.

## Defining providers

Every provider, including the one sessions start on, goes under
`agent.providers`. A provider kage already knows, such as `anthropic` or
`openai`, needs only a `credential`. Anything else needs a `baseUrl`, and a
`kind` when it speaks the Anthropic protocol rather than OpenAI's:

```json
{
  "agent": {
    "provider": "acme",
    "providers": {
      "acme": {
        "baseUrl": "https://llm.acme.example/v1",
        "kind": "openai",
        "credential": "...",
        "models": [{ "id": "coder", "contextWindow": 256000, "maxTokens": 32000 }]
      }
    }
  }
}
```

Provider ids use lowercase letters, digits and dashes, which is what kage
accepts. Listing a model the host's model store already knows does not add a
second copy. It adds what you wrote, such as a thinking level, to the existing
entry.

### Discovering models

Instead of listing models by hand, set `discover` and errand reads the
provider's `/models` listing at startup:

```json
"acme": {
  "baseUrl": "https://llm.acme.example/v1",
  "credential": "...",
  "discover": { "defaults": { "reasoning": true } },
  "models": [{ "id": "coder", "contextWindow": 256000 }]
}
```

`true` reads the usual OpenAI-shaped listing, with sizes under `context_window`
and `max_output_tokens`. For a listing shaped differently, say where things
are:

```json
"discover": {
  "path": "/api/models",
  "list": "result.models",
  "fields": { "id": "slug", "contextWindow": "top_provider.context_length" }
}
```

Discovered models sit underneath your `models` list. An entry you wrote for a
discovered model overrides only the fields it names. `defaults` fills fields
the listing does not carry. A provider that cannot be reached keeps the models
it lists, and the log says why.

`!models refresh` asks every provider again without a restart. Editing `models`
in the configuration file needs no refresh, because the file is reloaded when
it changes. A running sandbox keeps the models it was launched with.

## Usage windows

`!usage` and the bot's status show how much of each provider's usage window is
left. A session is refused up front when its provider's window is spent. A
provider is only read when its definition has `usage`:

```json
"zai-coding-cn": { "credential": "...", "usage": "zai" },
"acme": { "baseUrl": "https://llm.acme.example/v1", "credential": "...", "usage": "gateway" }
```

| Value | Reads |
| --- | --- |
| `"zai"` | z.ai's quota endpoint. |
| `"gateway"` | `/usage` under the provider's `baseUrl`. |
| An object | Wherever you point it, as below. |

```json
"usage": {
  "path": "/v1/usage",
  "auth": "bearer",
  "percent": "limiting.remainingPercent",
  "percentIs": "left",
  "resets": "limiting.resetsAt",
  "resetsAs": "iso"
}
```

| Field | Values |
| --- | --- |
| `auth` | `bearer`, or `raw` to send the key without `Bearer`. |
| `percentIs` | `used` or `left`. Required, because guessing wrong inverts the window. |
| `resetsAs` | `iso`, `ms`, or `s`. |

Paths are dotted keys. A key can pick a list entry by one of its fields, as
`windows[id=5h].usedPercent`, since list order is not guaranteed. A provider
that cannot be read shows as unknown and never blocks a session.

### Several windows

A provider with more than one window lists them under `windows`, the one to
show first at the top:

```json
"usage": {
  "path": "/usage",
  "windows": [
    { "percent": "windows[id=5h].usedPercent", "percentIs": "used",
      "resets": "windows[id=5h].resetsAt", "resetsAs": "iso" },
    { "percent": "windows[id=1w].usedPercent", "percentIs": "used",
      "resets": "windows[id=1w].resetsAt", "resetsAs": "iso" }
  ]
}
```

A window missing from the answer, or already past its reset, gives way to the
next. A spent window outranks all the others: if the weekly allowance is spent,
the provider reads as spent until that window resets, whatever the five hour
window says.

For a plan that keeps serving past a spent window and bills the overage to a
credit balance, set `"spentRefuses": false`. The windows still show in `!usage`,
but no longer refuse a session.

### Falling back when a window is spent

`agent.fallback` lists models to start on instead, in order, when the provider
a session would start on is spent:

```json
{ "agent": { "fallback": ["fast", "zai-coding-cn/glm-5.3-flash"] } }
```

The first one whose provider has room is used, and the thread says what was
spent and what the session runs on instead. The session is turned away only
when every fallback is spent too.

## Images for models that cannot see

When a session's model cannot read images, set `agent.visionModel` to one that
can. An attached image is described by that model first, and the agent is told
it is reading a description.

## Delegating a question

A session can also ask a cheaper model one question about one file or log,
without switching. See [delegation](/guide/delegation).
