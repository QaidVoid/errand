# Web interface

The daemon can serve a web interface from the same binary. Use it to read a
session as it happens, browse the project it works in, and start new sessions.

```json
{ "web": { "host": "127.0.0.1", "port": 8787 } }
```

Without a `web` section there is no listener at all.

## Who can reach it

The bind address is checked at startup:

| Allowed | Refused |
| --- | --- |
| Loopback, RFC 1918, link-local, unique-local, and the shared range tailnets use | Wildcard and public addresses, and host names |

A private overlay network such as a tailnet is the intended way to reach it
from a phone. A host name is refused because what it resolves to could change
after startup. An address with a leading zero in any part, such as
`010.0.0.1`, is refused as ambiguous.

::: danger Without a login, anyone who can reach it is an operator
They can start sessions, prompt any session, and run any command a thread owner
could. Set `"observer": true` to serve a read-only interface instead.
:::

If you set `publicUrl`, the daemon refuses to start unless there is a login or
the interface is read-only. A tunnel arrives over loopback, and the address
check cannot tell it apart from your own browser.

## Logging in with Discord

To publish the interface through a tunnel and still let people use it, add a
Discord application to log in with:

```json
{
  "web": {
    "host": "127.0.0.1",
    "port": 8787,
    "publicUrl": "https://errand.example.com",
    "login": { "clientId": "...", "clientSecret": "..." }
  }
}
```

The bot's own application works. In the Discord developer portal, under
OAuth2, copy the client id and secret and add `<publicUrl>/auth/callback` as a
redirect. Without `publicUrl`, the redirect is
`http://<host>:<port>/auth/callback`.

With a login:

- **Reading stays open** to anyone who can reach the interface, so a transcript
  linked from a pull request opens for everyone. Files served from a project
  have the configuration's secrets scrubbed, as transcripts do.
- **Acting needs a login**, and a logged-in visitor acts as their own Discord
  account, never as the operator.
- They must be a member of the server the bot's channel is in.
- `chat.allowedUserIds` and `chat.blockedUserIds` apply as in the channel, and
  are re-read on every request, so blocking somebody takes effect at once.
- In a session they have the same role they would have in its thread: owner,
  invited guest, or operator.

Membership is checked at login, and again when they act if the last check is
over ten minutes old. If Discord cannot be reached, nothing is done on their
behalf until it can. A login lasts seven days, and every login ends when the
daemon restarts.

## What it shows

- The conversation as numbered exchanges, so you can link to one.
- Each tool call together with its result, and edits as diffs.
- The agent's reasoning, folded away by default.
- Pictures and clips in place. They are kept in the session's record directory
  under `uploads`, so they outlive the session. Other files are offered as
  downloads.

Search narrows the view to exchanges containing a term and highlights it.
Filters narrow each exchange to what was said, what was run, or what changed,
each with a count.

Stopped sessions are listed too and read from their stored transcript. Sending
a message to one resumes it.

## What it does

- Anything you send is posted in the session's thread under your name, so the
  thread stays the full record for people reading it in Discord.
- A question the agent asks shows its choices as buttons. Pressing one answers
  it, the same as replying in the thread.
- A `!` command typed on the home page runs as it would in the channel. A
  listing such as `!usage` is answered under the box, and `!gen-image` or
  `!gen-speech` posts to the channel, addressed to you.
- A session started from the interface opens its thread from a channel message
  naming who started it. With `publicUrl` set, that message links back to the
  session.

Command answers and reactions are not shown. They belong to the thread they
were asked in.

## Themes

The interface ships kage's palettes: kage shadow and kage dawn, and kimi dark
and kimi light. System follows your desktop, using a dark and light pair you
can change under Appearance.

Your own theme starts from one of those and overrides some colours. It is the
same format as a kage theme file: `base` names the palette to start from, and
the `[gui]` table holds the colours under kage's token names. You can import a
kage theme as it is, and export one made here into `~/.config/kage/themes/`.
The terminal-only `[colors]` and `[groups]` tables are skipped. Themes you make
are stored in your browser, not on the daemon.
