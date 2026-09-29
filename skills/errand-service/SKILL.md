---
name: errand-service
description: >-
  Run the errand daemon as a system service with OpenRC or systemd: prepare
  the account and directories, install the binary and unit files, and set
  logging and resource limits. Use when the user wants errand to start on
  boot, run under a service manager, or asks about errand exit codes,
  restarts, logs, or per-session limits.
license: MIT
metadata:
  repo: https://github.com/QaidVoid/errand
---

# Errand as a service

Definitions for OpenRC and systemd live in `packaging/`. Both run the
binary as an ordinary account, never as root: the configuration holds the
bot token, and every agent starts as that same account, so files an agent
writes in a project belong to the person whose project it is. Complete the
errand-setup skill first: this skill assumes a built binary and a working
configuration file.

## 1. Prepare the account and the paths

```sh
useradd --system --home-dir /var/lib/errand --create-home errand

install -m 0755 target/release/errand /usr/local/bin/errand

install -o errand -g errand -m 0700 -d /var/lib/errand/.config/errand
install -o errand -g errand -m 0600 config.json \
        /var/lib/errand/.config/errand/config.json
```

Confirm: `/usr/local/bin/errand` runs, and the config file is mode 0600
owned by the service account. The daemon reads `~/.config/errand/config.json`
for that account first, so this placement needs no `ERRAND_CONFIG`.

## 2. Install the unit

OpenRC:

```sh
install -m 0755 packaging/errand.initd /etc/init.d/errand
install -m 0644 packaging/errand.confd /etc/conf.d/errand
$EDITOR /etc/conf.d/errand
rc-update add errand default
rc-service errand start
```

systemd:

```sh
install -m 0644 packaging/errand.service /etc/systemd/system/errand.service
$EDITOR /etc/systemd/system/errand.service
systemctl daemon-reload
systemctl enable --now errand
```

Confirm: the service is active and the log shows the startup sandbox
report, not a refusal. `packaging/README.md` repeats these steps beside the
files.

## 3. Respect the exit codes

Both definitions restart a crash and refuse to restart a refusal. Exits 2,
3, and 4 are decisions the daemon made, and repeating them would only log
the same line again. Code 4 would additionally fight the daemon that is
already serving, since two daemons on one bot token both act on every
message. When the service will not stay up, read the code first: 2 means a
refused configuration, backend, or token, 3 means an unenforceable
guarantee, 4 means the state directory is already held.

## 4. See more in the log

Each `-v` on the command line adds a level. Put the flag on `ExecStart` or
the init script command and restart the service.

- `-v` logs each decision and why: accepted messages, session starts and
  models, prompts sent, provider answers with status and time, refusals.
- `-vv` also logs every command sent to the agent and every event it
  answers with, by type.
- `-vvv` also logs every line exchanged with the agent verbatim, including
  prompts and tool output. Use it to diagnose one problem, then take it
  off.

## 5. Bound the resources

Both definitions limit the daemon and everything it starts as one tree, so
one busy session can spend the whole budget. Limiting each session
separately needs a cgroup the sandbox may create children in, named by
`BAILEY_CGROUP_ROOT`, which the daemon passes through to the sandbox tool.
Under systemd that is the service own delegated cgroup (`Delegate=yes`
with `DelegateSubgroup=supervisor`). Under OpenRC it has to be made and
delegated by hand, because OpenRC puts the daemon directly in the service
cgroup and one cgroup cannot both hold processes and hand its controllers
to children. Without one the daemon reports the gap at startup rather than
implying a limit it does not apply.
