# On GitHub

A session can be asked for on GitHub as well as in the channel. Mention the
bot's account on an issue or pull request, or assign one to it, and a session
starts in a thread in the channel, like any other, where it is watched and
steered. The issue is answered beside it, one comment per turn, and later
comments on the issue reach the same session.

```json
{
  "github": {
    "token": "...",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "trigger": { "allowedUsers": ["your-login"] }
  }
}
```

Without `trigger`, GitHub is only where work is sent, as pull requests.

## Who is heard

Only the logins in `allowedUsers`. On a public repository anyone can comment,
and every session runs code, so there is no wildcard. Anybody else's mention,
comment, or assignment is ignored, and an assignment counts only when somebody
listed made it.

The first person to ask owns the session, as in a thread. Somebody else listed
who comments on the issue is told they are not part of it until the owner
comments `!allow @their-login`.

## How it is noticed

The daemon reads the bot account's own GitHub notifications once a minute,
which is as often as GitHub allows. That covers any repository the account is
mentioned in, and needs a token that can read notifications: a classic token
with `repo` or `notifications`. A fine-grained token cannot.

Each notification is marked read once it has been handled, so the account's
notifications are the daemon's queue. Reading them yourself as that account
would take them from it. Anything said before the daemon started is not acted
on, as a chat message sent while it was down is not either.

## What happens

A mention on an issue with no session opens a thread in the channel, saying who
asked and linking the issue, and starts a session there in a workspace of its
own named after the repository and number, such as `parser-42`. The agent is
told the issue's title and link, and what the issue says when the asking was a
comment, and it clones the repository itself, as it would for any work.

After that, every comment from somebody listed is a prompt, mention or not, as
every reply in the thread is. Commands work the same way: `!model`, `!pr`,
`!stop`. Anybody who may take part in the thread steers it from there too.

Which thread answers which issue is kept in the state directory, so a restart
finds both again.

## What comes back

The thread shows everything, as it does for any session. The issue gets one
comment per turn, when it ends: what the agent said, and the line that closes
the turn. An answer to a command is its own comment, straight away.

A start that is turned away, for a spent usage window or a project that
already has a session, is said in the channel rather than on the issue: that
is for whoever runs the bot, not for everybody watching the repository.

## Trying it

1. Add `trigger` with your own login and restart the daemon. The log says
   `listening on GitHub` with the bot's login and who is heard; if the token
   cannot say whose it is, the log says that instead and GitHub stays unheard.
2. On an issue in a repository the bot account can see, comment
   `@the-bot say hello in a comment and change nothing`. A public repository
   works as it is; a private one needs the account added as a collaborator.
3. Within about a minute a thread opens in the channel and the session starts
   there. Run the daemon with `-v` to watch it in the log: `starting a
   session`, then `launching a sandbox`.
4. When the turn ends, the answer arrives in the thread and, as one comment,
   on the issue.
5. Comment again, without the mention, to continue. `!model` answers at once.

Mentioning the bot from a login not in `allowedUsers` should do nothing at all,
which is worth checking once too.
