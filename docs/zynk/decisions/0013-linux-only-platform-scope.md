# ADR 0013 — Linux x86_64 is the only platform zynk builds for

- **Status:** Accepted 2026-09-08 (operator decision relayed 2026-09-08)
- **Supersedes:** ADR 0012 (platform support tiers) in full — there are no tiers, no optional targets and no
  candidate-evidence manifest. **Narrows:** ADR 0011 §Decision (the per-platform atomic no-replace move).
  ADR 0010 (full fork) is unaffected.
- **Review date:** none. Restoring another platform requires a new ADR.

## Context

zynk's users are the team, on Fedora and Ubuntu on x86_64. Nobody on the team runs it on macOS or Windows.

ADR 0012 kept the cross-platform code and made the non-Linux targets optional, paid for by hosted evidence:
macOS static-archive alignment, Windows ConPTY, a Nix flake, a Homebrew tap, and a candidate-evidence
workflow with per-target `EVIDENCE.json` sidecars and a manifest that decided eligibility. That machinery
consumed the 3.1.0 release — the candidate was held repeatedly by macOS, aarch64 and Nix failures while the
Linux artifact was fine — and no user was on the far side of it. Optional code that nobody runs is also
unreviewed code: it is compiled, shipped and maintained on trust.

## Decision

1. **zynk builds for Linux x86_64 only.** Any other `target_os` fails at compile time with a `compile_error!`
   naming this ADR (`src/main.rs`). There is no fallback platform implementation.
2. **The non-Linux code is removed, not made optional.** Gone: the macOS and Windows `src/platform/` impls and
   the macOS input-source seam, the Windows client input paths, the Windows PTY / named-pipe IPC / remote
   stubs, the darwin archive-rewrite path in `build.rs` and `tests/darwin_archive_parser.rs`, the vendored
   `portable-pty` (Windows ConPTY patch — the registry crate is used directly), the platform-only
   dependencies, the PowerShell hook assets, the multi-target workflows (`release-dryrun.yml`,
   `build-artifacts-manual.yml`, `nix.yml`) and the release evidence/manifest scripts, and the Nix flake and
   package.
3. **Distribution is source-only.** Public distribution, if any, is crates.io. No support tiers, no release
   artifacts to verify, no Homebrew tap, no Nix flake. Internal builds are made locally from the exact
   reviewed SHA, with the SHA and the binary's sha256 recorded at install time.
4. **ADR 0011's atomic-move contract is narrowed to Linux.** The `zynk db adopt` / `backup` bundle move keeps
   exactly the `renameat2(RENAME_NOREPLACE)` arm; the macOS `renamex_np` and Windows `MoveFileExW` arms are
   gone. Refusing rather than copying when no atomic no-replace move exists is unchanged. The non-Unix
   `db_path_link` refusal path in `sqlite_effective_path` (ADR 0011 item 25) is gone with it — on Linux a
   symlinked database path is always resolved the way SQLite resolves it.
5. **The upstream port program excludes platform work.** Upstream macOS/Windows changes are classified SKIP,
   not ported back in. A port milestone never reintroduces a `target_os` branch.

## Consequences

- **Users lose** the macOS and Windows binaries. `cargo install zynk` on those targets now fails during the
  build with the `compile_error!` message above instead of producing a binary. Anyone who needs another
  platform runs an older release or a fork.
- **The gate collapses to one target.** CI is `check-required` (`just check` on Ubuntu) plus the
  conventional-commit and private-content gates; `just check` locally is the whole functional gate. There is
  no candidate-evidence dispatch, no `RELEASE_MANIFEST.txt`, no ELIGIBLE/BUILT_UNVERIFIED vocabulary, and no
  per-target release step to schedule.
- **The tree gets smaller and more honest**: every remaining line compiles and runs on the platform the team
  actually uses, and a `cfg(target_os)` in a diff is now a review finding rather than a pattern.
- Restoring macOS or Windows means writing the platform layer again against whatever the tree looks like then.
  That cost is accepted: it buys a release process one person can finish in an afternoon.
