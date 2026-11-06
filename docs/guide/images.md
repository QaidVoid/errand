# Images

errand can give sessions an image tool, let a model draw directly, and draw on
demand with `!gen-image`. All three use an endpoint that speaks the OpenAI
Images API (`POST /images/generations`).

## Giving sessions an image tool

Add `images` to one provider, naming the model every call uses:

```json
{
  "agent": {
    "providers": {
      "acme": {
        "baseUrl": "https://llm.acme.example/v1",
        "credential": "...",
        "models": [{ "id": "coder" }],
        "images": { "model": "image-small" }
      }
    }
  }
}
```

The agent then has kage's `generate_image` tool. A provider the agent already
uses for chat lends the tool its endpoint and key, as above, and so does one
kage knows by name, such as `openai`.

For an endpoint that serves images and no chat models, define a provider
without `models`. It needs a `baseUrl`:

```json
"pictures": {
  "baseUrl": "https://images.example/v1",
  "credential": "...",
  "images": { "model": "image-small" }
}
```

Its image models are never offered by `!model`, because it exists only for the
tool.

::: info Rules
- Only one provider may carry `images`.
- A provider that speaks the Anthropic API cannot, since that API has no images
  endpoint. This is checked against kage's own provider list at startup.
- A provider registered by a plugin cannot either.
- The tool needs kage 0.5.0 or later. An older kage ignores the setting.
:::

With the broker on, the image provider is brokered like any other. The sandbox
holds a stand-in for its key, never the key itself.

## Editing images

Add `editModel` to also give sessions kage's `edit_image` tool, on the same
endpoint (`POST /images/edits`):

```json
"images": { "model": "image", "editModel": "image-edit" }
```

The agent can edit a file in the project or, when it names none, the newest
picture in the thread, such as one it drew or one you attached. Editing the
newest picture needs a kage newer than 0.5.1.

## Using an image model as the session's model

An image model can also be picked with `!model` or `-m`, so each message goes
straight to it and returns one picture. List it under `models` with `produces`:

```json
"pictures": {
  "baseUrl": "https://images.example/v1",
  "credential": "...",
  "models": [{ "id": "image-large", "input": ["text"], "produces": ["text", "image"] }],
  "images": { "model": "image-small" }
}
```

Such a model reads only the latest message, has no tools, and remembers nothing
of earlier turns.

## What reaches the thread

Each picture, whether from the tool or drawn by a model in its own reply, is
posted as an upload captioned with its size, where it was saved, and anything
the image server said about it.

- Only PNG, JPEG, GIF and WebP are posted. Anything else is refused with a note
  in the thread.
- A picture larger than `output.maxUploadBytes` is named instead of posted.
- Set `output.postImages` to `false` to stop uploading pictures.

### Four per turn

A turn may produce at most four images and audio clips together. A fifth,
from any tool or from the model's own reply, interrupts the turn, even when
uploads are off. The interruption uses the same deadline as `!interrupt`: if
the agent does not stop within `timeouts.abortMs`, the session ends.

::: warning This is not a billing limit
Calls already in flight may still complete, and failed calls that return no
image cannot be counted.
:::

To turn the tool off entirely, add `"generate_image"` to `agent.deniedTools`
(spelled exactly). That does not affect `!gen-image`.

## `!gen-image`

`!gen-image <prompt>` draws one picture with the model `agent.imageGen` names.
It does not go through the agent. errand calls the provider's
`POST /images/generations` itself and posts what comes back.

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

`imageGen` is resolved like a `!model` name: an alias, `provider/id`, or a bare
id. A bare id must appear in some provider's `models` list, and a name two
providers list is refused. Naming the provider, as `pictures/image-large`,
works without a list.

The command works in a thread and in the channel. In the channel, the picture
is a reply to your message. Either way it is captioned with the prompt and the
model, opens no session, starts no sandbox, and does not change any session's
model.

### Effort

The effort is a thinking level, written as a flag or on the model name:

```
!gen-image --effort max a lighthouse
!gen-image image:max a lighthouse
```

Both send `image:max`, because the level is part of the model id the endpoint
receives. The flag wins when both are given. Without either, the level in
`agent.imageGen` is used, and without that the provider decides. The levels
are the same as for [models](/guide/models#thinking-levels).

A server that answers with a link instead of image bytes is refused, since
following it would mean a request to a host nobody configured.
