# Project rules

Rules for anyone working in this repository, human or otherwise. They override
default habits.

## Scope discipline

- Build what the change asks for and nothing more.
- Do not introduce an abstraction, interface, plugin point, or config knob to
  support a second implementation that does not exist yet. One backend means
  one concrete code path.
- If a change seems to require new abstraction, stop and ask before writing
  it.
- Prefer simple, readable, maintainable code. Reach for complexity only to
  gain performance that is actually needed, and say so explicitly when you do.

## Modules

- A module owns one thing and says what it is in a doc comment at the top.
- Nothing under `src/` imports from a sibling's internals. Modules talk
  through their exported surface.
- Anything that touches the outside world is injected, so a test needs no
  network, no container, and no clock.

## Comments

- Never narrate code. No inline comments restating what the next line does.
- Doc comments on exported items are expected.
- A comment is warranted only when it explains why something non-obvious is
  the way it is: a protocol quirk, a workaround, a security constraint.

## Character set

- ASCII only in source, config, docs, commit messages, and log output.
- No em-dashes. Do not substitute them with `--`, `-`, or `;`. Rewrite as
  proper, complete sentences.
- No smart quotes and no box-drawing characters, anywhere, ever.
- Chat output may use emoji, and only emoji, and only those in the one file
  that enumerates them. Each one carries a state; none is decoration.
- Logs stay ASCII even when the message they describe does not, so that
  grepping a log never depends on terminal font coverage.

## Version control

- Use `jj`. Never invoke `git` directly.
- Never push to a remote or open a pull request unless explicitly asked.

## Committing

- One coherent, working unit of work per commit. Not after every file touched,
  not one giant commit at the end.
- `cargo fmt --check`, `cargo clippy`, and `cargo test` pass before every
  commit.
- Commit messages use semantic format: `type: imperative message` or
  `type(scope): imperative message`, within 68 characters. Multi-line only
  when the body genuinely adds something.

## Dependencies

- Add dependencies with `cargo add`, never by hand-editing `Cargo.toml`.
- Every new dependency needs a reason that the standard library and the
  existing dependencies cannot meet.

## Errors

- Fail loudly with actionable messages. No silent catch-and-ignore.
- When an operation is retried or degraded, say so in the log and, where a
  person is waiting on it, where they are waiting.

## Tests

- Test logic that can break: parsing, chunking, routing, lifecycle
  transitions, admission.
- Do not write tests that only restate the implementation.
- Tests pass before a commit.
