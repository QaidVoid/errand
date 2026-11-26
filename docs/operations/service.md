# Running as a service

systemd and OpenRC definitions are in
[`packaging/`](https://github.com/QaidVoid/errand/tree/main/packaging). Both
run the daemon as an ordinary account, not root. Every agent runs as that same
account, so files an agent writes in a project belong to the account that owns
the project.

## Prepare the host

```sh
useradd --system --home-dir /var/lib/errand --create-home errand

git clone https://github.com/QaidVoid/errand
cd errand
(cd web && bun install && bun run build)
cargo build --release
install -m 0755 target/release/errand /usr/local/bin/errand

install -o errand -g errand -m 0700 -d /var/lib/errand/.config/errand
install -o errand -g errand -m 0600 config.json \
        /var/lib/errand/.config/errand/config.json
```

kage must be installed where sessions run: on the host's PATH for bailey, or
in the image for podman and Incus. For the Incus backend, also follow the
[host setup](/operations/incus#host-setup).

## systemd

```sh
install -m 0644 packaging/errand.service /etc/systemd/system/errand.service
$EDITOR /etc/systemd/system/errand.service
systemctl daemon-reload
systemctl enable --now errand
```

## OpenRC

```sh
install -m 0755 packaging/errand.initd /etc/init.d/errand
install -m 0644 packaging/errand.confd /etc/conf.d/errand
$EDITOR /etc/conf.d/errand
rc-update add errand default
rc-service errand start
```

## Updating the binary

A restart loses only the turns and background agents running and the
prompts waiting when it happens. A session's history and workspace outlive it, so an idle session
resumes as soon as somebody posts in its thread.

1. Put the new binary where the running one was started from, with
   `install` or `mv` rather than `cp`. A running binary cannot be written
   over in place, but it can be replaced by a new file at the same path.
2. Type `!restart` in the channel or any thread. It is for operators.

The bot then refuses new work and says so to whoever asks, waits for running
turns to finish, and for background agents a turn left at work, and starts the
new binary in its own place, with the same arguments and the same process id.
It says when it is back where `!restart` was typed.

It waits `timeouts.drainMs` at most, ten minutes unless configured. A turn or
background agent still running then is stopped, and once the bot is back its
thread is told and the session is asked, in its owner's name, to continue
where it stopped. A session whose background agents were stopped is told so,
since their results will not arrive. `!restart 30m` waits longer this once,
and `!restart now` does not wait.

`!drain` refuses new work in the same way and says when nothing is running,
background agents included, without restarting, for a restart done by hand.
`!undrain` ends either one, and calls off a restart that is waiting.

New work means a prompt or a new session. Commands still answer while the
bot drains, and `!ls`, `!cat`, `!file` and `!pwd` read the project from disk,
so they answer even in a thread whose session has ended without waking it.

## Exit codes

| Code | Meaning | Restarted? |
| --- | --- | --- |
| 0 | Stopped by a signal. | No |
| 1 | Something failed while running, such as the chat connection being gone too long. Sessions and sandboxes are shut down first. | Yes |
| 2 | The configuration, the sandbox backend, or the token was refused. | No |
| 3 | The backend cannot enforce a guarantee the configuration demands. | No |
| 4 | Another daemon already holds this state directory. | No |

Both definitions restart on 1 and not on 2, 3, or 4, since restarting would
only repeat the same refusal. Code 4 prevents two daemons on one bot token from
both answering every message. A lock left by a process that is gone is taken
over.

## Logging

The log holds progress, warnings, and errors by default. Each `-v` adds a
level:

| Flag | Adds |
| --- | --- |
| `-v` | Each decision and why: accepted messages, where a session starts and on which model, each prompt, each provider answer with status and time, each refusal. |
| `-vv` | Each step: every command sent to the agent and every event it answers with, by type. |
| `-vvv` | Every line exchanged with the agent, verbatim, including prompts and tool output. Use it only while diagnosing a problem. |

Under systemd, add the flag to `ExecStart` and restart the service.

## Resource limits

Both definitions apply memory, CPU, and process limits to the daemon and
everything it starts as one tree, so one busy session can use the whole budget.

To limit each session separately, the sandbox needs a cgroup it may create
children in, named by `BAILEY_CGROUP_ROOT`, which the daemon passes through.

- **systemd:** the unit's own delegated cgroup works, with `Delegate=yes` and
  `DelegateSubgroup=supervisor`.
- **OpenRC:** create and delegate one by hand. OpenRC puts the daemon directly
  in the service cgroup, and a cgroup cannot both hold processes and hand its
  controllers to children.

Without one, the startup report says per-session limits are not applied.
