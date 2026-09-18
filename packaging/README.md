# Running errand as a service

Two service definitions, one for OpenRC and one for systemd. Both run the
daemon from a checkout as an ordinary account, not as root: the configuration
file holds the bot token, and every agent is started as that same account, so
files an agent writes in a project belong to the person whose project it is.

## What to prepare

```sh
# The account the daemon runs as, with somewhere to keep its state.
useradd --system --home-dir /var/lib/errand --create-home errand

# The binary, built anywhere a Rust toolchain is installed. It carries the interface and
# its own runtime, so the host that runs it needs neither.
git clone https://github.com/QaidVoid/errand
cd errand && cargo build --release
install -m 0755 dist/errand /usr/local/bin/errand

# Its configuration, which holds the bot token, so it is readable by nobody
# else.
install -o errand -g errand -m 0700 -d /var/lib/errand/.config/errand
install -o errand -g errand -m 0600 config.json \
        /var/lib/errand/.config/errand/config.json
```

The daemon reads `~/.config/errand/config.json`, then `/etc/errand/config.json`,
then `config.json` in the working directory. `ERRAND_CONFIG` names one outright
and skips the search.

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

## What the exit codes mean

Both definitions refuse to restart on a refusal, since repeating it would only
log it again.

| Code | Meaning                                                          |
| ---- | ---------------------------------------------------------------- |
| 2    | the configuration, the sandbox backend, or the token was refused |
| 3    | the backend cannot enforce a guarantee the configuration demands |
| 4    | another daemon already holds this state directory                |

## Per-session limits

Both definitions limit the daemon and all of its sessions together. Limiting
each session separately needs a cgroup the sandbox may create children in,
named by `BAILEY_CGROUP_ROOT`, which the daemon passes through. Under systemd
that is the service's own delegated cgroup; under OpenRC it has to be made and
delegated by hand. Without one the daemon says so at startup rather than
implying a limit it is not applying.
