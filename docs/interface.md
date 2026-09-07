# The interface

An optional web interface reads a session as it happens, browses the project it
works in, and starts new ones. It is served by the daemon itself, from the same
binary.

```json
{ "web": { "host": "127.0.0.1", "port": 8787 } }
```

Absent means no listener at all.

## There is no login

Reaching it is the authorisation, so the address it binds to **is** the access
control, and it is checked rather than trusted. A wildcard or public address is
refused at startup, because a warning in a log is not a control. Loopback,
RFC 1918, link-local, unique-local, and the shared range a tailnet lives in are
allowed: reaching a machine over a private overlay network is the intended way
to use this from a phone.

A name is refused too. Resolving one would make what is reachable depend on what
DNS says at the moment the daemon started.

Anyone who can reach it acts with **operator authority**: they can start
sessions, prompt any session, and run any command a thread owner could. Set
`"observer": true` to serve one that can watch and read but change nothing,
which is what to publish through a tunnel.

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
