# The interface

An optional web interface reads a session as it happens, browses the project it
works in, and starts new ones. It is served by the daemon itself, from the same
binary.

```json
{ "web": { "host": "127.0.0.1", "port": 8787 } }
```

Absent means no listener at all.

## Who can reach it

The address it binds to is checked rather than trusted. A wildcard or public
address is refused at startup, because a warning in a log is not a control.
Loopback, RFC 1918, link-local, unique-local, and the shared range a tailnet
lives in are allowed: reaching a machine over a private overlay network is the
intended way to use this from a phone.

A name is refused too. Resolving one would make what is reachable depend on what
DNS says at the moment the daemon started.

Without a login, anyone who can reach it acts with **operator authority**: they
can start sessions, prompt any session, and run any command a thread owner
could. Set `"observer": true` to serve one that can watch and read but change
nothing.

## Logging in with Discord

To publish the interface through a tunnel and still let people use it, give it
a Discord application to log in through:

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

The bot's own application works. In the Discord developer portal, under OAuth2,
copy the client id and secret and add `<publicUrl>/auth/callback` as a redirect.
Without `publicUrl` the redirect is `http://<host>:<port>/auth/callback`.

Reading stays open to anyone who can reach the interface, so a transcript
linked from a pull request still opens for everyone. Starting or prompting a
session takes a login, and a logged-in visitor acts as their own Discord
account, never as the operator:

- They must be a member of the server the bot's channel is in. A wildcard in
  `chat.allowedUserIds` means anyone in the channel, and on the web the server
  stands in for the channel, so it cannot mean every Discord account there is.
- `chat.allowedUserIds` and `chat.blockedUserIds` apply as they do in the
  channel, and are read again on every request, so blocking somebody takes
  effect at once.
- In a session they are what they would be in its thread: the owner, a guest
  the owner invited with `!allow`, or an operator.

What anyone sends from the interface is posted in the session's thread under
their name, so the thread stays the whole conversation for the people reading
it in Discord.

Membership is checked when they log in. A login lasts seven days, and every
login ends when the daemon restarts.

## What it shows

The conversation as exchanges, numbered so one can be linked to. A tool call and
its result are one thing rather than two, and an edit is shown as a diff. What
the agent reasoned is folded away, since it is long and a conversation is not
the place for it.

Search narrows to the exchanges holding a term and marks it wherever it appears,
and says how many it found. The filters beside it narrow what is shown inside
each exchange: what was said, what was run, or what changed, each with a count
so it says what it will do before it is used.

A session that has stopped is listed too, and reads back from its stored
transcript. Sending to one picks the conversation up again.

## What it does not show

A command's answer, which belongs to the thread it was asked in, and reactions,
which are a chat affordance with no counterpart here. Both are shown where they
mean something.
