# On GitHub

A session can be asked for on GitHub as well as in the channel. Mention the
bot's account on an issue or pull request, or assign one to it, and a session
starts in a thread in the channel, like any other, where it is watched and
steered. The issue is answered beside it, one comment per turn, and later
comments on the issue reach the same session.

```json
{
  "github": {
    "token": "ghp_your_token_here",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "trigger": { "allowedUsers": ["your-login"] }
  }
}
```

Without `trigger`, GitHub is only where work is sent, as pull requests.

## Where and what starts a session

`repositories` narrows where the bot listens, as `owner/repo`, or `owner/*`
for every repository an account or organisation holds. Without it, the bot
listens wherever it is mentioned or assigned. A notification from anywhere
else is marked read and left alone.

`on` says what starts a session: `mention`, `assign`, or both, which is the
default. With `["assign"]`, a mention is still heard in an issue that already
has a session, but only assigning an issue to the bot starts new work.

```json
"trigger": {
  "allowedUsers": ["your-login"],
  "repositories": ["your-login/parser", "your-org/*"],
  "on": ["mention", "assign"]
}
```

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
asked and linking the issue, and starts a session there in a project
directory of its own, as every session has. The agent is
told the issue's title and link, and what the issue says when the asking was a
comment, and it clones the repository itself, as it would for any work.

After that, every comment from somebody listed is a prompt, mention or not, as
every reply in the thread is. Commands work the same way: `!model`, `!pr`,
`!stop`. Anybody who may take part in the thread steers it from there too.

Which thread answers which issue is kept in the state directory, so a restart
finds both again.

## What comes back

The thread shows everything, as it does for any session. The issue is told
only what each turn came to: the agent's last message, posted as one comment
when the turn ends. The agent is told so, and writes that message for the
issue's readers rather than commenting there itself. A turn that failed
before saying anything posts why instead, so whoever asked is not left
waiting. The working, command answers, and the turn's numbers stay in the
thread.

A start that is turned away, for a spent usage window or a model nothing
serves, is said in the channel rather than on the issue: that
is for whoever runs the bot, not for everybody watching the repository.

## How work is pushed

A pull request is pushed from the daemon's own copy of the project's git
directory, never from the tree the session writes. The copy is made first,
walked without following a single link, and everything the push reads, its
branch, its remote, its history, is read from that copy, so nothing the
session changes while the push runs can reach it. A link inside `.git` is
left out of the copy rather than followed.

A repository that borrows from outside itself is refused: an
`objects/info/alternates` file, a `commondir`, an `include` in its
configuration, a ref store named by path, or a partial clone, which fetches
what it lacks from another repository. Borrowed objects could make the push
publish another repository's commits under the bot's token, and borrowed
configuration could make the daemon read another file. A project cloned with
`--shared` or `--reference` borrows its objects this way. To push from one,
keep its objects in it with `git repack -a -d`, then delete
`.git/objects/info/alternates`. Each push copies the git directory, so a very
large history costs a moment and the disk space of one copy while it runs. A
git directory holding more than 2 GiB is not copied, and its pull request is
refused.

## What a session holds

Without `app`, a session holds the daemon's own token, in `GH_TOKEN`, and
`gh` and git use it. That token must be a classic one, since only a classic
token can read notifications, so a session holds read and write on every
repository the account can reach for as long as the token lives. The daemon
warns about this at startup.

With a GitHub App, each session holds an installation token minted for it
instead, and the daemon's token never enters the sandbox:

```json
{
  "github": {
    "token": "ghp_your_token_here",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "app": {
      "appId": 123456,
      "installationId": 7654321,
      "privateKeyPath": "/etc/errand/app.private-key.pem",
      "permissions": { "contents": "read", "issues": "write", "pull_requests": "read" },
      "repositories": ["errand"]
    }
  }
}
```

The token covers only the repositories the App was installed on, narrowed
further by `repositories` when they are named, carries only `permissions`
when they are named and everything the installation was granted when they
are not, and expires within the hour. The daemon mints it before the sandbox
starts, renews it ten minutes before it expires, and keeps it in the
session's state directory, where the `gh` wrapper reads it before every call,
so `gh` and git through it always present the current one. A `gh` called by
its full path skips the wrapper and is handed no token, though the session
can still read the file. A session whose token cannot be minted does not
start, and is never started on the daemon's token instead. A renewal that
fails is said in the thread and tried again every minute. The private key is
read at every mint, so a rotated key takes effect without a restart.
`!status` names the token's repositories, permissions, and how long it has
left.

### Making the App

The App is made once, on GitHub, by the account or organisation that owns
the repositories:

1. In **Settings, Developer settings, GitHub Apps**, choose **New GitHub
   App**. Give it a name and any homepage URL, and untick **Active** under
   **Webhook**: errand reads notifications with `token` and needs no
   webhook.
2. Under **Repository permissions**, grant what a session works with:
   **Contents** read, to clone and fetch, **Issues** read and write, to
   comment, **Pull requests** read, and **Actions** or **Checks** read to look
   at builds. **Metadata** read is always on. Contents write is not needed,
   since the daemon pushes with `token`.
3. On the App's page, note its **App ID**, then **Generate a private key**,
   which downloads a `.pem` file. Keep it where only the daemon's account can
   read it, outside `projectRoot` and `stateDir`, which a session can read and
   the configuration refuses, and name it in `privateKeyPath`.
4. **Install App** on the account or organisation, choosing the repositories
   sessions may reach. The number the installation's settings page ends in,
   as `/settings/installations/7654321`, is its `installationId`.

`permissions` in the configuration can ask a token for less than the App was
granted, never more. A session's first `!status` shows what it was given.

Notifications, pull requests and pushes keep using `token`. An installation
token cannot write outside its installation, so forks, and pull requests on a
repository the App is not installed on, are opened by the daemon with
`token`, through the pull request request file, as always.

Whichever credential a session holds, its `gh` refuses `gh pr create`,
`gh repo fork`, `gh repo delete`, `transfer` and `archive`, `gh release
delete`, `gh workflow disable`, setting or deleting secrets and variables,
defining an alias, printing the token with `gh auth token` or
`--show-token`, and `gh api` with `DELETE`, `PATCH` or `PUT`, on a transfer
or fork route, or for another host than github.com, each with what to do
instead. It does not read GraphQL mutations. That catches the habit, not a
determined session, which can read its credential and reach the API without
`gh`, or edit `gh`'s own configuration in its state directory. The wall is
the credential: an App token holds only the permissions it was granted, and
GitHub refuses everything else. When the session ends every token it held
that has not expired is revoked, and its file removed. `privateKeyPath` must lie outside `projectRoot` and
`stateDir`, since a session that could read the key would hold the App.

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
4. When the turn ends, the whole of it is in the thread, and its answer
   arrives on the issue as one comment.
5. Comment again, without the mention, to continue. `!model` answers at once.

Mentioning the bot from a login not in `allowedUsers` should do nothing at all,
which is worth checking once too.
