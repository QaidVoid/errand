# Configuration and data

Every field is listed in the [configuration reference](/reference/configuration).
This page covers how changes are applied and what errand keeps on disk.

## Where the file is read from

1. `~/.config/errand/config.json`
2. `/etc/errand/config.json`
3. `config.json` in the working directory

`ERRAND_CONFIG` names a file directly and skips the search. An unknown key is
refused rather than ignored, so a misspelling is caught at startup. Inside a
provider definition, which may carry keys of your own, only misspellings of the
broker's route keys are refused.

Add the schema to your file for editor completion and checking:

```json
{ "$schema": "https://raw.githubusercontent.com/QaidVoid/errand/main/config.schema.json" }
```

## Changing it while running

The daemon watches its configuration file and applies changes without a
restart. The next session, and the daemon's own answers to `!usage`, `!model`
and the rest, use the new file.

A file that cannot be used is not fatal once the daemon is running. A
half-written edit, a missing field, or a path that does not resolve is refused
whole, every reason is logged, and the daemon keeps serving under the previous
configuration. Only a configuration that cannot be read at startup stops it.

### What needs a restart

- The chat connection and token, so who may use the bot is fixed until restart.
- The web interface's address and login.
- The state directory.
- The sandbox backend and, under podman or Incus, its image.
- Sessions already running, which keep what they launched with.

The rest of the sandbox section, the egress broker, and its provider routes
are rebuilt on reload, so the next session launches under them. The log names
any changed setting that waits for a restart.

## What is kept on disk

Each session leaves a state directory, a record (its transcript, grants, and
uploads), and its own project directory. Provider call receipts are kept in
`receipts.db`, and memory in `memory.db`, both under `stateDir`.

### Automatic cleanup

A session unused for 30 days is collected: its state directory, record,
project, thread index entry, and receipts all go together. "Unused" means the
later of when it last started and when its transcript was last written.

```json
{ "retention": { "enabled": true, "olderThan": "30d", "every": "1d" } }
```

| Field | Meaning |
| --- | --- |
| `enabled` | Whether the daemon collects at all. `errand threads gc` works either way. |
| `olderThan` | How long a session goes unused before it is collected. |
| `every` | How often the daemon checks, besides once at startup. |

Lengths are a number and a unit, `s`, `m`, `h`, `d`, `w`, `mo` or `y`, alone
or combined, as `7d 12h`. A month is 30 days and a year 365. Retention
settings are re-read at the next check, without a restart.

Ending a session with `!stop` prevents it from resuming, but keeps its index
entry so retention can still collect its data. Sessions stopped before this
behavior was introduced remain unindexed and need manual cleanup with
`errand threads gc --unindexed`. Preview with `--dry-run` first.

Some things are never collected automatically:

- A running session, however old.
- A project named by hand in an older version of errand, or a session project
  such a name took over.
- Anything the thread index does not name, including entries explicitly
  forgotten, evicted, or lost when the index was reset. The daemon will not
  guess what such a session left.

### Cleaning up by hand

| Command | Does |
| --- | --- |
| `errand threads list` | List remembered threads, most recent first. |
| `errand threads show <thread>` | Show one thread, what it holds on disk, and its recorded provider calls. |
| `errand threads forget <thread>` | Stop resuming it, and keep its data. |
| `errand threads remove <thread> --yes` | Forget it and delete its data, its own project included. |
| `errand threads prune` | Forget threads whose data is already gone. |
| `errand threads revive <session> --thread <id> --owner <id>` | Put a forgotten thread back. |
| `errand threads gc --dry-run` | Show what would be collected. Add `--older-than` to try another window. |
| `errand threads gc` | Collect now. Add `--unindexed` to also collect state the index no longer names. |

`gc` and `remove` refuse while the daemon is running, because the daemon owns
the thread index and alone knows which sessions are live. Stop it first.
`gc --dry-run` only reads, so it works either way.
