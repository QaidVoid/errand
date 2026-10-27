# Generating audio

A provider whose endpoint answers the OpenAI speech API
(`POST /audio/speech`) can give sessions a speech tool. Mark it with `speech`,
naming the model every call asks for, and optionally the voice used when the
agent names none:

```json
{
  "agent": {
    "providers": {
      "voices": {
        "baseUrl": "https://tts.example/v1",
        "credential": "...",
        "speech": { "model": "tts", "voice": "alloy" }
      }
    }
  }
}
```

The shapes are the ones [images](images.md) take. A provider the agent already
uses for chat lends the tool its endpoint and its key, and so does one kage
knows by name. A provider kage does not know, which serves speech and no chat
model, is described without `models`, as above, and needs a `baseUrl`. With
`sandbox.egress.mode` set to `proxy` it is brokered like any other provider.

Only one provider may carry `speech`, and the same provider may carry `images`
too. One that speaks the Anthropic API cannot, and neither can one a plugin
registers.

The tool needs a kage newer than 0.5.1. An older kage ignores the setting, and
its sessions run without the tool.

## In the thread

The agent calls kage's `generate_speech` tool, and kage hands the clip back
with the call's result. Audio a model speaks into its own reply is handled the
same way. Each clip is posted to the thread as an upload, captioned with its
size, where the tool also saved it in the project when it did, and what the
server said about it. Discord plays MP3 and WAV inline.

The daemon checks that the bytes are MP3 or WAV before anything is posted, and
refuses anything else, telling the thread why. A clip larger than
`output.maxUploadBytes` is named instead of posted. Setting `output.postAudio`
to `false` stops clips from being uploaded, as `output.postImages` does for
pictures.

Images and clips share one budget of four per turn. A turn that generates a
fifth, through `generate_image`, `generate_speech`, or in the model's own reply,
is interrupted, and the next prompt tells the agent why. The rules are the ones
[images](images.md) describe.

To refuse the agent's speech tool entirely, set `agent.deniedTools` to include
`"generate_speech"`. This does not disable the human-invoked `!gen-speech`.

## Music

A `music` object on one provider, naming a `model`, gives sessions kage's
`generate_music` tool, for an endpoint that answers
`POST /music/generations`. The shapes are the ones `speech` takes:

```json
"songs": {
  "baseUrl": "https://music.example/v1",
  "credential": "...",
  "music": { "model": "music" }
}
```

Only one provider may carry `music`, and it may be the one carrying
`images` or `speech`. A song is posted to the thread like a clip, under
`output.postAudio` and the same four per turn. A song can take a minute or
more. A song the server made but kage could not fetch comes back as a link
in the agent's reply instead. Denying `"generate_music"` in
`agent.deniedTools` turns the tool off. It needs a kage newer than 0.5.1.

## `!gen-speech`

`!gen-speech <text>` reads a text aloud with the model `agent.speechGen` names,
and posts the clip as an upload. Like `!gen-image`, it does not go through the
agent: errand calls the provider's own `POST /audio/speech` and posts the bytes
that come back.

```json
{
  "agent": {
    "speechGen": "voices/tts",
    "providers": {
      "voices": {
        "baseUrl": "https://tts.example/v1",
        "credential": "...",
        "speech": { "model": "tts" }
      }
    }
  }
}
```

The name is resolved as `!gen-image` resolves `agent.imageGen`: an alias, a
`provider/id`, or a bare id the provider lists under `models`. It takes no
effort level. Speech tiers are separate model names on the server, such as
`tts` and `tts-max`, so pick one by name.

Two flags may come before the text, in either order:

```
!gen-speech --voice alloy --language en good morning, everyone
```

`--voice` names the voice, and the server picks one when it is left out.
`--language` is a hint some servers take. A server that refuses it, or refuses
the MP3 format errand asks for, is asked again without the field it names. A
flag after the text is read aloud as part of it. `/gen-speech` offers `voice`
and `language` as their own options.

The command works in a thread and in the channel, as `!gen-image` does. In the
channel the clip is a reply to the message that asked. Either way it opens no
session, launches no sandbox, and does not count toward a turn's four.
