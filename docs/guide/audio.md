# Audio

errand can give sessions a speech tool and a music tool, and read text aloud on
demand with `!gen-speech`. The setup mirrors [images](/guide/images).

## Speech

A provider that answers the OpenAI speech API (`POST /audio/speech`) can carry
`speech`, naming the model and optionally a default voice:

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

Sessions then have kage's `generate_speech` tool. As with images, a chat
provider can lend the tool its endpoint and key, and a provider with no
`models` is an endpoint of its own that needs a `baseUrl`.

- Only one provider may carry `speech`. It may be the same one that carries
  `images`.
- An Anthropic-speaking provider cannot, and neither can a plugin's provider.
- It needs a kage newer than 0.5.1.

## Music

A `music` object on one provider gives sessions kage's `generate_music` tool,
for an endpoint that answers `POST /music/generations`:

```json
"songs": {
  "baseUrl": "https://music.example/v1",
  "credential": "...",
  "music": { "model": "music" }
}
```

Only one provider may carry `music`, and it may also carry `images` or
`speech`. A song can take a minute or more to generate. When the server
returns a link on its own host, the daemon fetches it from that host only and
posts it like any other clip. Music needs a kage newer than 0.5.1.

## What reaches the thread

Clips are posted as uploads, captioned with their size, where they were saved,
and what the server said. Discord plays MP3 and WAV inline.

- Only MP3 and WAV are posted. Anything else is refused with a note in the
  thread.
- A clip larger than `output.maxUploadBytes` is named instead of posted.
- Set `output.postAudio` to `false` to stop uploading clips and songs.

Clips and images share the [four per turn](/guide/images#four-per-turn) limit.

To turn a tool off, add `"generate_speech"` or `"generate_music"` to
`agent.deniedTools`.

## `!gen-speech`

`!gen-speech <text>` reads text aloud with the model `agent.speechGen` names
and posts the clip. Like `!gen-image`, it calls the provider directly and does
not go through the agent.

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

The name resolves like `agent.imageGen`. It takes no effort level, because
speech tiers are separate model names on the server, such as `tts` and
`tts-max`.

Two optional flags go before the text, in either order:

```
!gen-speech --voice alloy --language en good morning, everyone
```

| Flag | Meaning |
| --- | --- |
| `--voice` | The voice to use. The server picks one when it is left out. |
| `--language` | A hint some servers accept. |

A server that refuses `--language`, or the MP3 format errand asks for, is asked
again without that field. A flag after the text is read aloud as part of it.

The command works in a thread and in the channel, opens no session, and does
not count toward a turn's four.
