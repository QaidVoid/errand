# In a thread

A message in the served channel opens a thread and starts a session. Everything
after that happens in the thread.

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
extends rather than a message per call, and a diff after each edit. A turn ends
with a line that names you, so a phone tells you when it is your turn again.

Reactions on your own message track its fate: accepted, then succeeded, failed,
or interrupted. There is only ever one, so scrolling back reads as final state
rather than as a history of transitions.

## Attachments

Files you attach are saved into the session's project, under `attachments/`,
and the agent is told where they went. A name that aims outside the project
cannot get there: it is reduced to one path segment and then resolved by the
same rule that confines the agent.

An image is handed to the model as well as saved. If the session's model cannot
see images, a cheaper one that can is asked to describe it first, and the agent
is told plainly that it is reading a description. See [two models](/models).

## When a session ends

Idling out, crashing, or a daemon restart all leave the thread open and the
session resumable: the agent's own history outlives its sandbox, so posting in
the thread picks the conversation back up where it stopped.

Only `!stop` finishes with a thread, and only then is it archived. A thread that
drops out of the sidebar because a session idled out is one its author then has
to go hunting for.
