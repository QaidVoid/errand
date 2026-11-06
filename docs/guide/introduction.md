# Introduction

errand is a daemon that runs a coding agent from a chat channel. You post a
message, errand opens a thread for it, and a sandboxed session starts working
in a project directory of its own. Everything the agent says, runs, and changes
comes back to that thread, and every reply you post there is the next prompt.

The agent is [kage](https://github.com/QaidVoid/kage). errand is what sits
around it: the chat connection, the sandbox, the queue, the network broker,
and the record of what happened.

## How a session works

1. A message in the served channel opens a thread and starts a **session**.
2. The session gets a fresh project directory under `projectRoot` and a state
   directory under `stateDir`, and nothing else on the filesystem.
3. A **sandbox** starts with kage inside it. Its network goes through a broker
   that holds the real provider key, so the sandbox only ever sees a stand-in.
4. The agent's replies, tool calls, and diffs stream into the thread. A reply
   sent while a turn is running redirects that turn.
5. When the session idles out, its sandbox stops. The conversation is kept, so
   posting in the thread later picks it back up.

Several sessions run at once, one per thread. A queue limits how many turns
reach the model provider at the same time, so a burst of messages cannot get
an account rate limited.

## What it guarantees

**The agent is always sandboxed.** There is no unsandboxed mode and no flag to
turn one on. When the host cannot enforce something the configuration asks for,
errand says which guarantee is missing and refuses to start. See
[sandboxing](/operations/sandboxing).

**The chat token never enters a sandbox.** The provider key does not either
when the egress broker is on, which is the default. The sandbox holds a
per-session stand-in that only its own broker accepts.

**One project, one agent.** Two sessions never write the same working tree.

**Nothing is published on your behalf.** A pull request is composed and pushed
by the daemon, and only when somebody in the thread asks for one with `!pr`.

::: warning Scrubbing is damage control, not a boundary
Text a session reports is scrubbed of every configured secret of 8 bytes or
more before it reaches the channel, the web interface, or the transcript on
disk, and an upload that contains one is refused. An agent that re-encodes or
splits a key gets past this. The boundary is the sandbox and the broker.
:::

## What it does not do

errand does not review the agent's work or decide anything for you. A session
is a conversation with a model that can run commands. errand makes that
conversation happen where you already are, and makes the commands run
somewhere they cannot reach the rest of your machine.

## Where to go next

- [Quick start](/guide/quick-start) gets a daemon running.
- [Working in a thread](/guide/threads) covers what you can type and what comes
  back.
- [Sandboxing](/operations/sandboxing) explains what each backend confines.
