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
cd errand && deno task build
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
