# As a service

Definitions for OpenRC and systemd are in
[`packaging/`](https://github.com/QaidVoid/errand/tree/main/packaging). Both run
the binary as an ordinary account, not as root: the configuration file holds the
bot token, and every agent is started as that same account, so files an agent
writes in a project belong to the person whose project it is.

## What to prepare

```sh
useradd --system --home-dir /var/lib/errand --create-home errand

git clone https://github.com/QaidVoid/errand
cd errand && cargo build --release
install -m 0755 dist/errand /usr/local/bin/errand

install -o errand -g errand -m 0700 -d /var/lib/errand/.config/errand
install -o errand -g errand -m 0600 config.json \
        /var/lib/errand/.config/errand/config.json
```

## OpenRC

```sh
install -m 0755 packaging/errand.initd /etc/init.d/errand
install -m 0644 packaging/errand.confd /etc/conf.d/errand
$EDITOR /etc/conf.d/errand
rc-update add errand default
rc-service errand start
```

## systemd

```sh
install -m 0644 packaging/errand.service /etc/systemd/system/errand.service
$EDITOR /etc/systemd/system/errand.service
systemctl daemon-reload
systemctl enable --now errand
```

## Restarting, and not restarting

Both definitions restart a crash and refuse to restart a refusal. Exit 2, 3, and
4 are decisions the daemon made, and repeating them would only log the same line
again; 4 would also mean fighting the daemon that is already serving. See
[getting started](/start) for what each code means.

## Seeing more in the log

The log holds progress, warnings, and errors by default. Each `-v` on the
command line adds a level:

- `errand run -v` logs each decision and why: which messages were accepted,
  where a session starts and on which model, each prompt sent, each provider
  answer with its status and time, and each refusal.
- `errand run -vv` also logs each step: every command sent to the agent and
  every event it answers with, by type.
- `errand run -vvv` also logs every line exchanged with the agent, verbatim.
  That includes prompts and tool output, so keep it to diagnosing a problem.

Under systemd, put the flag on `ExecStart` and restart the service.

## Limits

Both apply memory, cpu, and process limits to the daemon and everything it
starts, as one tree. That means one busy session can spend the whole budget.

Limiting each session separately needs a cgroup the sandbox may create children
in, named by `BAILEY_CGROUP_ROOT`, which the daemon passes through. Under
systemd that is the service's own delegated cgroup (`Delegate=yes` with
`DelegateSubgroup=supervisor`). Under OpenRC it has to be made and delegated by
hand, because OpenRC puts the daemon directly in the service cgroup and a cgroup
cannot both hold processes and hand its controllers to children.

Without one, the daemon says so at startup rather than implying a limit it is
not applying.
