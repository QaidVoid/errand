# Generating images

A provider whose endpoint answers the OpenAI Images API
(`POST /images/generations`) can give sessions an image tool. Mark it with
`images`, naming the model every call asks for:

```json
{
  "agent": {
    "providers": {
      "gateway": {
        "baseUrl": "https://gateway.example/v1",
        "credential": "...",
        "models": [{ "id": "chat-model" }],
        "images": { "model": "image-small" }
      }
    }
  }
}
```

A provider the agent already uses for chat lends the tool its endpoint and its
key, as above, and so does one kage knows by name, such as `openai`. A provider
kage does not know, which serves images and no chat model, is described without
`models`:

```json
"pictures": {
  "baseUrl": "https://images.example/v1",
  "credential": "...",
  "images": { "model": "image-small" }
}
```

It is written for the image tool alone, so `!model` never offers its image
models, and it needs a `baseUrl`: without one there is nowhere to send a call,
and sessions run without the tool. With `sandbox.egress.mode` set to `proxy` it
is brokered like any other provider, so the sandbox holds a nonce in place of
its key.

Only one provider may carry `images`. An `anthropic` provider cannot, since that
API has no images endpoint, and neither can kage's own `openai-responses`
provider, or one a plugin registers.

The tool needs kage 0.5.0 or later. An older kage ignores the setting, and its
sessions run without the tool.

## An image model as the session's model

An image provider's models can also be chosen with `!model` or `-m`, so a
message goes straight to the image model, with no chat model in between. List
them under `models` with `produces`, and each message makes one picture:

```json
"pictures": {
  "baseUrl": "https://images.example/v1",
  "credential": "...",
  "models": [{ "id": "image-large", "input": ["text"], "produces": ["text", "image"] }],
  "images": { "model": "image-small" }
}
```

Such a model reads only the latest message, has no tools, and remembers
nothing of earlier turns. Some gateways answer with a link to the picture
rather than the picture: kage fetches it when the link is on the provider's
own host, which needs a kage newer than 0.5.0.

## In the thread

The agent calls kage's `generate_image` tool, and kage hands the picture back
with the call's result. A model that draws on its own, such as a Gemini image
model, hands its pictures back in its reply instead. Either way each picture is
posted to the thread as an upload, captioned with its size, where the tool also
saved it in the project when the agent asked it to, and what the image server
said about it, such as that it fell back to another model.

The daemon checks that the bytes are a PNG, JPEG, GIF or WebP before anything
is posted. Anything else is refused, and the thread is told that a picture was
not posted, and why. A picture larger than `output.maxUploadBytes` is named
instead of posted. A turn posts at most four. Setting `output.postImages` to
`false` turns the uploads off.

## `!gen-image`

`!gen-image <prompt>` draws one picture with the model `agent.imageGen` names,
and posts it as an upload. It does not go through the agent: errand calls the
provider's own `POST /images/generations` with the model and the prompt, and
posts the bytes that come back. That is the difference between a picture and a
link: a chat model asked for an image answers in prose, which is not one.

```json
{
  "agent": {
    "imageGen": "pictures/image-large",
    "providers": {
      "pictures": {
        "baseUrl": "https://images.example/v1",
        "credential": "...",
        "images": { "model": "image-large" }
      }
    }
  }
}
```

The name is read as `!model` reads one, so an alias or a bare id works too. The
provider is the one whose `models` list the model appears on, and its `baseUrl`
is where the request goes. A bare id is not read as the provider a session runs
on, which is a chat host and serves no images route. A name two providers list
is refused, naming both, rather than guessed at. A level on the name stays on
it, since it is part of the model id the endpoint is asked for: `image:max` is
sent as written.

The provider must list the model under `models` for a bare name to find it:

```json
"pictures": {
  "baseUrl": "https://images.example/v1",
  "credential": "...",
  "models": [{ "id": "image-max", "input": ["text"], "produces": ["text", "image"] }]
}
```

Naming the provider in `imageGen`, as `pictures/image-max`, resolves without the
list, because the name says where the model lives.

The command works in a thread and in the channel. In a thread it draws into the
thread; in the channel it draws into the channel, as a reply to the message
that asked, addressed to whoever asked it. A picture posted bare in a shared
channel belongs to nobody, and nobody can see what it answers. Either way it
opens no session, launches no sandbox, and leaves the session's model alone.

## Effort

The effort is a thinking level, and there are two ways to ask for one:

```
!gen-image --effort max a lighthouse
!gen-image image:max a lighthouse
```

Both put `image:max` in the request, since the level is part of the model id the
images endpoint is asked for. A flag wins when both are given, being the more
explicit of the two. `/gen-image` offers the effort as its own option, and
writes it as the flag.

With neither given, the level written into `agent.imageGen` is used, and with
none there the provider decides. A level asked for on the command replaces the
configured one for that picture rather than stacking beside it.

The levels are `off`, `minimal`, `low`, `medium`, `high`, `xhigh` and `max`. One
that is not a level is refused before anything is drawn, since a provider would
either reject it or ignore it.

A server that answers with a link instead of bytes is refused rather than
followed, since fetching it would be a second request to a host nobody
configured. An `anthropic` provider cannot be used, because that API has no
images endpoint.
