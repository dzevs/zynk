# zynk — agent & contributor guide

zynk is a terminal workspace manager for AI coding agents (AGPL-3.0-or-later), a fork of
[herdr](https://github.com/ogulcancelik/herdr). Build requires Rust (stable) + **Zig 0.15.2**; the TS
asset test needs Bun. (Claude: see `CLAUDE.md`.)

## Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState`
  is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only
  draws. Never mutate state during render.
- **No god objects.** Split modules that do too much; `app/` is split into state, actions, and input.
- **Platform code is isolated.** OS-specific behavior lives in `src/platform/`; core modules avoid `#[cfg(target_os)]`.
- **Detection is evidence-based.** The detector reads a screen snapshot, never the parser or viewport. Encode
  invariant vs alternative visible controls as explicit AND/OR gates; don't match incidental whole-pane text.
- **Reuse UI patterns.** zynk is a mouse-first TUI; follow the existing modal/screen/affordance language.

## Testing

Use `just` recipes:

```bash
just test    # cargo nextest + maintenance script tests
just check   # formatting check + tests
```

Run `just check` before committing; don't bypass failing checks. Unit tests live next to the code
(`#[cfg(test)] mod tests`). New `AppState`/`Workspace` behavior should be testable with `AppState::test_new()`
and `Workspace::test_new()` without PTYs. For broad refactors touching core surfaces, persisted state,
protocol/API IDs, identity, restore/handoff, or detection authority, add or name characterization tests first.

## Vendored libghostty-vt

`vendor/libghostty-vt.vendor.json` records the vendored upstream source commit. Local patches are tracked in
`vendor/libghostty-vt.patches.md` and stored under `vendor/patches/libghostty-vt/`. `just check` verifies the
patch index. The bundled sources keep their upstream license/provenance — see `vendor/libghostty-vt/LICENSE`.

## Code conventions

- Rust: no `unwrap()` in production code. Use `tracing` for logging. `#[allow]` only with a justifying comment.
- Platform-specific code must be compile-gated (`#[cfg(unix)]`/`#[cfg(windows)]`); put OS APIs in `src/platform/`.
- Don't add dependencies without a reason; check existing ones first.

## Commit style

Lowercase conventional commits, no emojis, no AI co-author lines. When a change relates to a GitHub issue, add
a `refs #<issue>` body line. Keep subjects descriptive — they feed release notes.

## Contributions

Contributions are welcome via GitHub issues and pull requests — see `CONTRIBUTING.md`. The maintainer
(`dzevs`) reviews and merges; this public repo is a curated export, so accepted changes may be re-applied and
re-exported by a maintainer.
