# errand

Run a coding agent from a chat channel, in a sandbox it cannot escape.

A message in one configured channel opens a thread and starts a session. The
agent works in a project directory of its own and nowhere else. Replies in
the thread are prompts; what the agent says, runs, and changes comes back to the
same thread, and can be read again later in a browser.

## What it is for

Driving a coding agent from wherever you are, on somebody else's machine, with
the isolation written down rather than assumed. One channel, one thread per
session, several sessions at once.

## What it guarantees

- **The agent is sandboxed.** There is no unsandboxed mode, and no flag that
  turns it off. If the backend cannot enforce what the configuration asks for on
  this host, the daemon says which guarantee it cannot keep and refuses to start
  rather than pretending.
- **The chat token never enters a sandbox.** The provider credential does,
  unless the broker stands in for it, because the agent needs it. The text a
  session reports is scrubbed of every configured secret of 8 bytes or more
  before it reaches a channel, a browser, or the transcript on disk, and an
  upload that holds one is refused. That is damage control, not a boundary:
  an agent that re-encodes a key defeats it.
- **A project has one agent.** Two sessions never write the same working tree.
- **Nothing is opened on your behalf.** A pull request is composed by the daemon
  and only when somebody in the thread asked for one.

## What it does not do

It does not review the agent's work, and it does not decide anything for you. A
session is a conversation with a model that can run commands; the value here is
that the conversation happens where you already are and the commands happen
somewhere they cannot reach your machine.
