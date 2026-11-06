# Working in a thread

Each session lives in one thread. This page covers what you can post there,
who may post, and what comes back.

## Starting a session

Any message from an allowed account in the served channel opens a thread and
starts a session in a fresh project directory under `projectRoot`. The whole
message is the first prompt.

If people also talk in that channel, set `chat.startOnMention` so only a
message that mentions the bot starts anything:

```
@errand fix the failing test
```

The mention is removed before the agent sees the prompt. Inside a thread you
never need to mention the bot. Commands such as `!help`, `!usage` and `!model`
are answered in the channel whatever the setting says.

## What a message means

| You post | What happens |
| --- | --- |
| Plain text | A prompt. If a turn is running, it **redirects** that turn instead of queueing a new one. |
| `!!! text` | An aside. Everyone in the thread sees it, and the agent is never told. |
| `!command` | A command. See the [command reference](/reference/commands), or type `!help`. |
| An unknown `!word` | Ignored, so commands meant for other bots in the channel are not sent to the model. |

To queue a prompt for after the running turn instead of redirecting it, use
`!then <prompt>`.

## Who may do what

- **The owner** is whoever started the thread. The owner can stop the session,
  open pull requests, and invite others.
- **Guests** are invited by the owner with `!allow @somebody`. They can prompt
  the agent and read the project, but cannot end the session or change who
  takes part.
- **Operators** are listed in `chat.operatorUserIds` and may control any
  session.

Somebody who was not invited is told so once. Their messages still get a
reaction, so nobody wonders whether a message arrived.

`!shutdown` is not a session command. It powers off the host, so it is off
unless `shutdown.allowedUserIds` names somebody, and no session role grants it.

## What comes back

- The agent's words as it writes them.
- Its tool calls, grouped into one message that grows instead of one message
  per call.
- A diff after each file edit.
- Pictures and audio clips it generates, as uploads. See [images](/guide/images)
  and [audio](/guide/audio).
- A closing line when the turn ends that mentions you, so your phone tells you
  it is your turn again.

A model that pastes a base64 picture into its reply does not flood the thread.
The long run is replaced by a note of its size.

### Reactions

The reaction on your own message tracks what became of it: accepted, then
succeeded, failed, or interrupted. Only one is shown at a time, so scrolling
back shows the final state. The [character reference](/reference/characters)
lists each one.

## Attachments

Files you attach are saved into the session's project under `attachments/`,
and the agent is told where they are. A file name is reduced to a single path
segment, so it cannot point outside the project.

Images are also handed to the model. If the session's model cannot see images,
a cheaper one that can describes the picture first, and the agent is told it is
reading a description. Picture stickers arrive as images, and Lottie stickers
arrive as the JSON that draws them.

## Deleting a message

Deleting a message in Discord withdraws it from the session:

- It is removed from the transcript and the web view, which mark where a
  message was withdrawn.
- It is removed from the agent's stored conversation, so a resumed session
  never sends it to a model again.
- A running session is told to disregard it.

What deletion cannot do:

- Unsend a message the model has already read. The agent's earlier replies
  stay as they were.
- Undo work the message caused. A file it asked for stays written.
- See a deletion made while the daemon was disconnected. Discord does not
  replay those. Say so in the thread once the bot is back, or start a new
  session.

## When a session ends

An idle timeout, a crash, or a daemon restart stops the sandbox but leaves the
thread open. The agent's history outlives the sandbox, so posting in the thread
resumes the conversation where it stopped.

Only `!stop` finishes a session for good and archives its thread.

Idle timeouts and restarts are not announced, because the resumed session says
so when you come back. A failure is announced, with the reason.

Old sessions are cleaned up automatically after 30 days without use. See
[configuration and data](/operations/configuration#what-is-kept-on-disk) to
change that or clean up by hand.
