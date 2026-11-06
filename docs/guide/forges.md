# GitHub and Forgejo

A session can be started from an issue or pull request as well as from the
channel. Mention the bot's account, or assign the issue to it, and a session
starts in a new thread in the channel, where you can watch and steer it. Each
turn's answer is posted back to the issue as one comment.

## Setting up

Give errand the bot account's token and commit identity under `forge`, and say
who may ask for work under `trigger`:

```json
{
  "forge": {
    "token": "YOUR_FORGE_TOKEN",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "trigger": { "allowedUsers": ["your-login"] }
  }
}
```

`forge.type` defaults to `github`. For Forgejo, add the instance:

```json
{
  "forge": {
    "type": "forgejo",
    "url": "https://forge.example.com",
    "token": "YOUR_FORGE_TOKEN",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "trigger": { "allowedUsers": ["your-login"] }
  }
}
```

Use HTTPS for a Forgejo `url`, since the token travels on every request.

Without `trigger`, the forge is only where work goes, as pull requests opened
with `!pr`.

::: info The old `github` section
`github` still works but is deprecated. Rename it to `forge` and leave `type`
out. Do not set both.
:::

## Choosing what starts a session

```json
"trigger": {
  "allowedUsers": ["your-login"],
  "repositories": ["your-login/parser", "your-org/*"],
  "on": ["mention", "assign"]
}
```

| Field | Meaning |
| --- | --- |
| `allowedUsers` | The logins that are heard. Required, and `*` is refused. |
| `repositories` | Where to listen, as `owner/repo` or `owner/*`. Without it, anywhere the bot is mentioned or assigned. |
| `on` | `mention`, `assign`, or both (the default). With `["assign"]`, a mention still reaches an issue that already has a session, but only an assignment starts new work. |

Only listed logins are heard. On a public repository anyone can comment, and
every session runs code, so there is no wildcard. An assignment counts only
when a listed login made it. On Forgejo, only direct assignments to the bot
count, not team assignments.

The first person to ask owns the session, as in a thread. Another listed login
who comments is told they are not part of it until the owner comments
`!allow @their-login`.

## How the bot notices

The daemon reads the bot account's notifications once a minute. Each one is
marked read once handled, so the account's notifications are the daemon's
queue. Reading them yourself while logged in as the bot takes them away from
it.

On GitHub this needs a classic token with `repo` or `notifications`, because a
fine-grained token cannot read notifications. Activity from before the daemon
started is not acted on. On Forgejo, unread activity from before a restart is
skipped too.

## What happens on the issue

A mention on an issue with no session opens a thread in the channel, saying
who asked and linking the issue. The agent is told the issue's title and link,
plus the comment that asked, and clones the repository itself.

From then on, every comment from a listed login is a prompt, mention or not.
Commands work there too: `!model`, `!pr`, `!stop`. People in the thread can
steer the same session from the channel. Which thread belongs to which issue is
kept in the state directory, so a restart finds both again.

The thread shows everything. The issue gets only the agent's last message of
each turn, as one comment, and the agent is told to write that message for the
issue's readers. A turn that fails before saying anything posts why instead.

A session that cannot start, because a usage window is spent or no provider
serves the model, is reported in the channel rather than on the issue.

## Scoped tokens with a GitHub App

Without an App, a session holds the daemon's own token in `GH_TOKEN`. That is a
classic token, so the session can read and write every repository the account
can reach, and the daemon warns about this at startup.

With a GitHub App, each session gets its own short-lived installation token,
and the daemon's token never enters the sandbox:

```json
{
  "forge": {
    "token": "YOUR_FORGE_TOKEN",
    "userName": "errand-bot",
    "userEmail": "bot@example.com",
    "app": {
      "appId": 123456,
      "installationId": 7654321,
      "privateKeyPath": "/etc/errand/app.private-key.pem",
      "permissions": { "contents": "read", "issues": "write", "pull_requests": "read" },
      "repositories": ["parser"]
    }
  }
}
```

The installation token:

- covers only the repositories the App is installed on, narrowed further by
  `repositories`
- carries only `permissions` when they are given, and everything the
  installation has when they are not
- expires within the hour, and is renewed ten minutes before it does
- is revoked when the session ends

`!status` shows the token's repositories, permissions, and remaining time. A
session whose token cannot be minted does not start. It never falls back to
the daemon's token. A failed renewal is reported in the thread and retried
every minute. The private key is read at every mint, so a rotated key takes
effect without a restart.

Notifications, pushes, and pull requests still use `forge.token`, since an
installation token cannot write outside its installation.

### Creating the App

1. Under **Settings, Developer settings, GitHub Apps**, choose **New GitHub
   App**. Any homepage URL works. Untick **Active** under **Webhook**, since
   errand does not use webhooks.
2. Under **Repository permissions**, grant **Contents** read, **Issues** read
   and write, **Pull requests** read, and **Actions** or **Checks** read if
   sessions should look at builds. Contents write is not needed, because the
   daemon pushes with `forge.token`.
3. Note the **App ID**, then **Generate a private key**. Store the `.pem` file
   where only the daemon's account can read it, outside `projectRoot` and
   `stateDir`. The configuration refuses a key inside either, since a session
   that could read it would hold the App.
4. **Install App** on the account or organisation and choose its repositories.
   The number at the end of the installation's settings URL, as
   `/settings/installations/7654321`, is the `installationId`.

`permissions` can ask for less than the App was granted, never more.

### What `gh` refuses

Whichever token a session holds, its `gh` wrapper refuses commands that publish
or destroy, each with a hint about what to do instead: `gh pr create`,
`gh repo fork`, `gh repo delete`, `transfer` and `archive`,
`gh release delete`, `gh workflow disable`, changing secrets or variables,
defining an alias, printing the token, and `gh api` with `DELETE`, `PATCH` or
`PUT`, on transfer or fork routes, or against another host.

This catches habits, not a determined session, which can read its token and
call the API directly. The real limit is the token: an App token only has the
permissions it was granted.

## How pull requests are pushed

`!pr <title>` asks the daemon to open a pull request for the branch the agent
is on, with that title:

```
!pr fix the trailing line in the parser
```

The repository is found a few levels down in the session's project directory,
where the agent cloned it. When a session holds more than one clone, pick one
with `!pr --repo <dir> <title>`. The push is made from the daemon's own copy of the project's git directory,
never from the tree the session writes, so nothing the session does while the
push runs can affect it.

A repository that borrows from outside itself is refused: one with
`objects/info/alternates`, a `commondir`, an `include` in its configuration, a
ref store named by path, or a partial clone. A clone made with `--shared` or
`--reference` borrows objects this way. To push from one, run
`git repack -a -d` and delete `.git/objects/info/alternates`. A git directory
over 2 GiB is not copied, and its pull request is refused.

## Trying it

1. Add `trigger` with your own login and restart the daemon. The log says
   `listening on GitHub` with the bot's login and who is heard.
2. On an issue the bot can see, comment
   `@the-bot say hello in a comment and change nothing`. A private repository
   needs the bot added as a collaborator.
3. Within a minute a thread opens in the channel. Run the daemon with `-v` to
   watch `starting a session` and `launching a sandbox` in the log.
4. When the turn ends, its answer arrives on the issue as one comment.
5. Comment again, without the mention, to continue.

Check once that a mention from a login not in `allowedUsers` does nothing.
