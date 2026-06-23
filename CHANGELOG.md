# Changelog

## [3.0.1] — 2026-06-23

Source crate on crates.io: `cargo install zynk` now installs the native Zynk terminal app **from source**. It
builds with Rust (stable) and **Zig 0.15.2**, and requires **network access during the build** — after Cargo
fetches the crate, the build fetches libghostty-vt's Zig package dependencies into Zig's global cache
(`cargo install --offline` is not supported). For a no-build install, prefer the prebuilt binaries (GitHub
Releases / Homebrew) or Nix. This is a packaging-only release — app behavior is identical to 3.0.0; the
GitHub Release binaries and Homebrew formula remain at 3.0.0.

## [3.0.0] — 2026-06-20

First public installable release of the **native Zynk terminal app** (AGPL-3.0-or-later), a fork of
[herdr](https://github.com/ogulcancelik/herdr) with a net-new multi-agent conversation layer (global SQLite
persistence, native protocol metadata + a visible message header, honest delivery/receipt, and hybrid
retrieval) on top of the inherited terminal-multiplexer base (workspaces / tabs / panes / agent awareness).

This is an early, evolving release — expect rough edges.

**Downloads** ([GitHub Releases](https://github.com/dzevs/zynk/releases)): prebuilt binaries for
`linux-x86_64`, `linux-aarch64` (GNU/glibc dynamic, **glibc ≥ 2.30**), `macos-x86_64`, `macos-aarch64`, and
`windows-x86_64`, plus `SHA256SUMS`. The macOS and Windows binaries are **unsigned** (clear the macOS
quarantine with `xattr -dr com.apple.quarantine`; use Windows SmartScreen "Run anyway"). Homebrew
(`brew install dzevs/tap/zynk`), Nix (`nix run github:dzevs/zynk`), and a source build (Rust + Zig 0.15.2)
also work.

**Deferred:** `cargo install zynk` is not the native app yet (the crates.io `zynk` is the retired 2.x), and
self-update/auto-update — both planned.

**Lineage:** the 2.x `zynk` crate on crates.io was a separate, now-retired ACP portable protocol/helper CLI
(MIT); the native app continues the name on the 3.x line under AGPL-3.0-or-later.
