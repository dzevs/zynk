---
description: Audit release readiness against the root CHANGELOG and README before a zynk release
---
Audit release readiness for the zynk public repo (single repo, canonical branch `main`).

Optional starting ref override: `$1`
Extra user intent/context: `${@:2}`

READ-ONLY by default. Never run a release, tag, `cargo publish`, or push — those are operator-gated (`WORKFLOW.md`).

Process:

1. Determine the base ref.
   - If `$1` is a ref/tag, use it. Otherwise the latest release tag (zynk tags are `vX.Y.Z`, e.g. `v3.0.1`):
     ```bash
     git describe --tags --abbrev=0
     ```

2. Inspect the range `base..HEAD`.
   ```bash
   git log --first-parent --reverse --format='%H%x09%s' <base>..HEAD
   git log --reverse --format='%H%x09%s%n%b' <base>..HEAD   # full commit bodies when needed
   ```

3. Detect merged PRs.
   - Look for first-parent subjects that indicate merges, including squash subjects like `title (#123)`.
   - Treat a merged PR as the primary release unit; do **not** also list the individual commits inside it.
   - If `gh` is available and the PR number is known, fetch the PR title/body for context.

4. Handle direct commits separately — any commit in the range not represented by a merged PR stands on its own.

5. Infer what matters.
   - For each PR or direct commit, inspect changed files and diff stats; read the key files in full when needed.
   - Ignore pure housekeeping unless it has release value: version bumps, release/tag commits, changelog-only
     commits, formatting-only changes, comment/doc-only changes that do not materially affect users.

6. Audit the root `CHANGELOG.md` — the project's single, in-repo changelog.
   - It uses `## [X.Y.Z] — YYYY-MM-DD` sections, newest first. The release being prepared is either a new top
     section or the accumulated changes since the latest released section.
   - Compare user-facing shipped changes in the range against the changelog. Flag missing entries for new
     features, bug fixes, removals, breaking changes, changed defaults, user-visible command/config/API
     behavior, and security-relevant changes.
   - Do not require entries solely for internal client/server protocol version bumps; mention protocol only
     when the release intentionally changes user-facing compatibility beyond the normal restart requirement.
   - Inspect commit bodies for issue references (`refs #<n>`) and GitHub closing keywords (`fixes`/`closes`/
     `resolves #<n>`) — these close issues once they land on `main`. List them under the issue-references output.
   - For each merged external human PR, check the entry credits the PR number and thanks the contributor in the
     existing style, e.g. `(#129, thanks @user)` (both issue + PR when useful, e.g. `(#128, #129, thanks @user)`).
     Do not add thanks for maintainer bots/automation accounts.
   - Flag stale entries (no matching shipped change in the range) and entries too implementation-focused for end
     users. Preserve the existing sections: `Added`, `Changed`, `Fixed`, `Removed`, and `Breaking Changes`.

7. Audit the root `README.md` — the project's single, in-repo public doc.
   - Compare user-facing changes in the range against the README: new/changed commands, config keys, supported
     agents, integrations, defaults, compatibility notes, and the install snippets / version pins (the `vX.Y.Z`
     download URLs + the Homebrew / crates.io / Nix lines).
   - Flag README sections that disagree with the implementation or with the version being released.

8. Check version + packaging consistency.
   - `Cargo.toml` `version` must match the intended tag (`vX.Y.Z`), the top `CHANGELOG.md` section, and the
     README version pins (the prebuilt-binary/Homebrew version can intentionally trail the crates.io source
     version — confirm the README says so rather than assuming a mismatch is a bug).
   - If the release changes `Cargo.lock` or the version, refresh the Nix `cargoHash` in `nix/package.nix`; a
     stale hash fails the `nix` workflow and `nix flake check` with a fixed-output-derivation mismatch. Use the
     `got:` hash printed by `nix flake check --print-build-logs`.
   - Confirm `just check` is green and `just gate` is clean; confirm `LICENSE` + `NOTICE` (AGPL-3.0-or-later +
     the upstream herdr attribution) are intact.

9. Apply changes only when explicitly asked.
   - Do not edit files during the audit unless the user asks you to apply fixes.
   - When applying fixes, keep changes scoped to the root `CHANGELOG.md` / `README.md` / version files named
     here. Never tag, publish, or push — recommend those as operator-gated next steps.

Output format:

```md
Release readiness: READY | NOT READY

Base: <tag>   Range: <tag>..HEAD   Meaningful shipped changes: yes | no

Version consistency: Cargo.toml <x> · CHANGELOG <x> · README pins <x> · intended tag v<x> → CONSISTENT | MISMATCH

Changelog: OK | MISSING ENTRIES | NEEDS ATTENTION
Missing:
- <user-facing shipped changes missing from CHANGELOG.md>
Wrong or questionable:
- <stale or unclear entries, if any>

README: OK | MISSING | INACCURATE
- <user-facing gaps or stale sections>

Nix cargoHash: OK | NEEDS UPDATE | NOT CHECKED
Gates: just check <green|red> · just gate <clean|fail> · LICENSE/NOTICE <intact|issue>

Issue references the release will close:
- #<n>

Accepted / no action:
- <items the user explicitly accepted>

Required before release (all operator-gated):
1. <short action>
```

Keep the output glanceable. Put commit inventories, excluded housekeeping, and commands run in an appendix only
when they materially help the operator. If the range has no meaningful user-facing changes, say so plainly
instead of forcing entries.
