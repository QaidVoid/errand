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
