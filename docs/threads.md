# In a thread

A message in the served channel opens a thread and starts a session, which
works in a project directory of its own under `projectRoot`, named after the
session. Everything after that happens in the thread. A message that opens
with a word and a colon, as `demo: fix the test`, no longer picks a project
by that name, as errand once did: it is sent to the agent whole, as the
prompt.

If the channel is also somewhere people talk, set `chat.startOnMention` and only
a message that names the bot starts anything:

```
@errand fix the failing test
```

The mention summons it and is not part of what you asked, so it is taken out
before the agent sees it. Inside a thread nothing has to be named: the session
is the conversation. Nor does a command: `!help`, `!usage` and `!model` are
addressed to the bot already, and are answered in the channel whatever the
setting says.

## What a message means

A reply is a prompt. If a turn is already running, what you say **redirects
it**, which is the point of saying it now rather than waiting: it is the same
turn, so it neither opens a new one nor takes another slot in the queue.

A message starting `!!!` is an **aside**. Everyone in the thread sees it and the
agent is never told, so you can talk about a session in front of it.

A message starting `!` is a command. An unknown one is left alone entirely: a
shared channel usually has more than one bot in it, and forwarding somebody
else's command to a model costs money to no purpose.

The full list is in [commands](/reference/commands), or type `!help`.

## Who may do what

Whoever starts a thread owns it. The owner can invite others with
`!allow @somebody`, which lets them prompt the agent and read the project but
not end the session or change who takes part. Somebody who was not invited is
told once, and every message they send still gets a reaction, so nobody is left
wondering whether it arrived.

Operators, named in the configuration, may control any session.

`!shutdown` is not a session command at all. It acts on the machine, so no
session role grants it and the daemon answers it against its own list.

## What comes back

The agent's words as it produces them, the commands it runs as one block that
extends rather than a message per call, and a diff after each edit. A picture
the agent generates or draws arrives as an upload, captioned with its size (see
[generating images](/images)). A turn ends with a line that names you, so a
phone tells you when it is your turn again.

A model that answers with a picture inside its text, as base64, does not turn
into hundreds of messages: the long run is replaced by a note of its size.

Reactions on your own message track its fate: accepted, then succeeded, failed,
or interrupted. There is only ever one, so scrolling back reads as final state
rather than as a history of transitions.

## Attachments

Files you attach are saved into the session's project, under `attachments/`, and
the agent is told where they went. A name that aims outside the project cannot
get there: it is reduced to one path segment and then resolved by the same rule
that confines the agent.

Stickers are taken the same way as files. A picture sticker arrives as an image
the agent is shown, and a Lottie sticker arrives as the JSON that draws it.

An image is handed to the model as well as saved. If the session's model cannot
see images, a cheaper one that can is asked to describe it first, and the agent
is told plainly that it is reading a description. See [two models](/models).

## Deleting a message

Deleting a message in Discord withdraws it here too. Its text is taken out of
the session's transcript and the web view, which show that a message was
withdrawn rather than closing the gap silently, and out of the agent's own
stored conversation, so a session resumed later never sends it to a model
again. A session still running is told to disregard it.

Two things it does not do. A message already sent to a model may already have
been read and acted on, and nothing here unsends it: the agent's earlier
replies stand, because rewriting them would be inventing a past. And work the
message caused is not undone, so a file it asked for stays written.

One gap worth knowing: a deletion that happens while the daemon is disconnected
from Discord is never delivered, and Discord does not replay it. That message
stays. If it mattered, say so in the thread once the bot is back, or stop the
session and start another.

## When a session ends

Idling out, crashing, or a daemon restart all leave the thread open and the
session resumable: the agent's own history outlives its sandbox, so posting in
the thread picks the conversation back up where it stopped.

Only `!stop` finishes with a thread, and only then is it archived. A thread that
drops out of the sidebar because a session idled out is one its author then has
to go hunting for.

An idle timeout, or a daemon restart, is not announced at all. The next message
picks the session up and the resumed session says so, which leaves a notice
nothing to add. A failure is announced, because it stopped part way through
something and you should know why.

## What is kept, and for how long

A session leaves its state directory, its record and its own project on disk.
Once a session has gone unused for `retention.olderThan`, 30 days by default,
the daemon collects all of it together: the state directory, the record, the
project, its entry in the thread index, and the receipts of its provider
calls. Unused is
judged by its last activity, the later of when it last started and when its
transcript was last written, which every turn does. A project named by hand,
as errand once allowed with a `name:` prefix, is never collected, and neither
is a session's own one that such a project took over. A running session is never collected, however old its entry. A thread that
comes back while a sweep runs is either reopened before the sweep reaches it,
and then kept, or waits until the sweep is done with it, and then finds
nothing to resume. The index entry goes last, so a sweep that fails part way
leaves it naming what remains for the next one.

The daemon looks when it starts and every `retention.every`, a day by
default, and logs each session it collects with what it freed. A change to
any retention setting is read at the next look, with no restart: `enabled`
set to `false` stops it, and a new `every` sets the wait after that look.

```json
{
  "retention": { "enabled": true, "olderThan": "30d", "every": "1d" }
}
```

A length is written as a number and a unit, `s`, `m`, `h`, `d`, `w`, `mo` or
`y`, alone or together, as `7d 12h`. A month is 30 days and a year 365.

The daemon only collects what its thread index names. A thread ended with
`!stop` leaves the index at once, and so does one evicted or reset, and the
daemon leaves what such a session kept alone, since an index that went
missing would otherwise make every session look abandoned.
`errand threads gc --unindexed` collects those too, by their own age, with
the session's own project when no remembered thread works in it.

To see what would go without deleting anything, run
`errand threads gc --dry-run`, with `--older-than` to try another window. To
collect by hand, stop the daemon and run `errand threads gc`: the daemon owns
the thread index while it runs and alone knows which sessions are live, so the
command refuses while it holds its lock, as `errand threads remove` does.
`errand threads remove <thread>` deletes one session at once, its own project
included.
