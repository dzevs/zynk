# Contributing to zynk

Thanks for your interest in contributing.

zynk is a fork of [herdr](https://github.com/ogulcancelik/herdr), an opinionated terminal workspace
manager for AI coding agents. Contributions are welcome via GitHub issues and pull requests.

## Before you start

- **Understand your code.** Using AI to help write code is fine; submitting code you can't explain is not.
- **Keep the scope small** and consistent with zynk's existing design and interaction patterns.
- For larger changes to UI, behavior, persistence, protocol, or architecture, open an issue to discuss the
  approach before sending a big PR.

## Building and testing

Requires Rust (stable), **Zig 0.15.2** (the bundled `libghostty-vt` is built with Zig), and Bun (for the
TypeScript asset test).

```bash
just install-hooks   # one-time: installs the fmt pre-commit hook
just ci              # fmt --check + clippy -D warnings + tests (run before opening a PR)
```

Don't open a PR that bypasses failing tests, formatting, or build errors.

## Commits

Use lowercase [conventional commits](https://www.conventionalcommits.org/), no emojis, no AI co-author lines.
If a PR relates to an issue, add a `refs #<issue-number>` line in the commit body.

## Bug reports

Use the bug report issue template and include: current behavior, expected behavior, the shortest
reproduction, the affected zynk version/commit, OS, and terminal.

## Note on this repo

`dzevs/zynk` is a curated public mirror of a private source-of-truth repository. Accepted contributions are
reviewed and merged by the maintainer (`dzevs`); larger changes may be re-applied upstream of the export and
re-exported. A maintainer will guide you if needed.

## Security

Report security issues privately — see [`SECURITY.md`](./SECURITY.md).
