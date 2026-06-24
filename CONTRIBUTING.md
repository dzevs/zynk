# Contributing to zynk

Thanks for taking the time to contribute. Contributions of any size are welcome.

zynk is a terminal workspace manager for AI coding agents, built in Rust with a
ratatui TUI, portable-pty PTYs, tokio async, and Unix-socket IPC. It's a fork of
[herdr](https://github.com/ogulcancelik/herdr) and is distributed under the
AGPL-3.0-or-later license.

> [!TIP]
> New to open source? The
> [first-contributions guide](https://github.com/firstcontributions/first-contributions)
> walks through the GitHub fork-and-pull-request flow.

Read [`DEVELOPMENT.md`](./DEVELOPMENT.md) to set up an isolated dev environment
before you start coding.

## Contributor guidelines

This guide covers what to do when you:

- [Found a bug](#did-you-find-a-bug)
- [Want to open a pull request](#do-you-want-to-open-a-pull-request)
- [Want to add or change a feature](#do-you-want-to-add-or-change-a-feature)

## Before you start

- **Understand your code.** Using AI to help write code is fine; submitting code
  you can't explain isn't.
- **Keep the scope small** and consistent with zynk's existing design and
  interaction patterns.
- For larger changes to the UI, behavior, persistence, protocol, or
  architecture, open an issue to discuss the approach before sending a big PR.

## Did you find a bug?

- Search the [GitHub issues](https://github.com/dzevs/zynk/issues) first to
  confirm the bug isn't already reported.
- If no open issue covers the problem, open a new bug report with a
  [minimal reproduction](#minimal-reproduction).

Include a clear title and description, the current behavior, the expected
behavior, the shortest reproduction, and your environment: the zynk version or
commit (`zynk --version`), OS, and terminal.

## Do you want to open a pull request?

Follow [`DEVELOPMENT.md`](./DEVELOPMENT.md) to build zynk and run its tests.
After validating your change against an isolated dev runtime, open a pull request
against `main` with `gh`:

```bash
gh pr create --fill
```

Requirements for every PR:

- **Link the issue it addresses.** Reference it in the PR description (for
  example, `Fixes #1234` or `Closes #1234`). A PR without a linked issue may be
  closed.
- **Describe the problem and the solution.** State what changed and why.
- **Pass the full local check.** Run `just check` (or `just ci`) and confirm
  it's green before you open the PR. Don't open a PR that bypasses failing
  tests, formatting, or build errors.

A maintainer reviews each PR and either requests changes or merges it.

## Do you want to add or change a feature?

- Open a feature request issue and wait for feedback from the maintainers.
- Once the direction is agreed, open a pull request that tracks the work so the
  implementation can be discussed in context.

## Minimal reproduction

A minimal reproduction is the shortest sequence of steps that demonstrates a bug
with the least setup. It isolates the issue, proves the bug isn't caused by your
wider environment, and often surfaces the root cause on its own.

A good zynk reproduction includes:

- The exact `zynk` command or key sequence that triggers the problem.
- The starting state: a clean config, or the specific `config.toml` keys
  involved.
- The agent or integration involved, if the bug is agent-specific (for example,
  pane detection or footer injection).
- The observed behavior versus the expected behavior.

Keep out anything unrelated: your wider workspace layout, unrelated config keys,
or features that don't bear on the bug. For a TUI or rendering issue, attach a
screenshot or an `asciinema` recording. For a server or IPC issue, attach the
relevant `tracing` log lines.

## Commits

Use lowercase
[conventional commits](https://www.conventionalcommits.org/) — for example,
`fix: clamp cursor row on resize`. No emojis, and no AI co-author or
`Co-authored-by` lines. When a commit relates to an issue, add a
`refs #<issue-number>` line in the commit body.

The `commit-msg` git hook enforces the conventional-commit format. Install the
hooks once per checkout:

```bash
just install-hooks
```

## Keeping private content out

`dzevs/zynk` is the canonical public repository. A two-layer private-content gate
runs locally (through `just install-hooks` → the pre-commit hook) and in CI:

- `scripts/check_public_tree.py` — a structural gate over tracked paths.
- `.gitleaks.toml` — a content gate for secrets and maintainer-private strings.

Run the gates yourself with `just gate`. Keep maintainer-private paths and
strings out of every commit. On a gate failure, stop and fix the root cause —
never bypass the gate.

## Licensing

zynk is licensed under the GNU Affero General Public License v3.0 or later
(AGPL-3.0-or-later), the same license as upstream herdr. By contributing, you
agree that your contribution is licensed under AGPL-3.0-or-later as part of the
combined work.

Preserve the upstream copyright and the herdr attribution carried in
[`NOTICE`](./NOTICE) and [`LICENSE`](./LICENSE) in all builds — the license
demands it.

## Security

Report security issues in private — see [`SECURITY.md`](./SECURITY.md). Don't
file a public issue for a vulnerability.

## More context

- [`DEVELOPMENT.md`](./DEVELOPMENT.md) — dev environment, building, and testing.
- [`CLAUDE.md`](./CLAUDE.md) — architecture overview and repository conventions.
- `docs/zynk/` — the deeper design decisions and the architecture-decision
  records that govern the project.
