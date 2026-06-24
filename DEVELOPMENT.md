# zynk development guide

This guide covers building, testing, and iterating on zynk. Read it before you
open a pull request — [`CONTRIBUTING.md`](./CONTRIBUTING.md) covers the
contribution flow, and this guide covers the dev environment.

zynk is a Rust + ratatui TUI workspace manager for AI coding agents, with
portable-pty PTYs, tokio async, and interprocess Unix-socket IPC. The CLI is a
thin client over a local socket server; most commands return JSON.

## Prerequisites

- **Rust** (stable, via `rustup`). The toolchain is pinned through
  `Cargo.lock`; build with `--locked`.
- **Zig 0.15.2.** The bundled `libghostty-vt` terminal library is built from
  source with Zig at `cargo build` time. The exact version matters — other Zig
  releases won't compile the vendored source.
- **Bun.** Needed only for the TypeScript asset test
  (`just test-ts`), which checks the integration assets shipped to agents.
- **`just`.** The task runner. Every command below runs through it.

Optional, used by some `just` recipes:

- **gitleaks** — the content half of the private-content gate (`just gate`).
- **vale** — the prose linter for docs (`just docs-lint`).
- **Python 3** — runs the maintenance-script unit tests in `just check`.

## Getting started

Clone the repository and build the release binary:

```bash
git clone https://github.com/dzevs/zynk.git
cd zynk
just build
```

`just build` runs `cargo build --release --locked`. The first build is slow: the
`build.rs` script invokes `zig build` to compile the vendored `libghostty-vt`
into a static library before rustc links it. Later builds reuse the Zig cache
and only relink when the vendored source changes.

Install the git hooks once per checkout so the pre-commit gate (lint + the
private-content gates) and the `commit-msg` conventional-commit gate are active:

```bash
just install-hooks
```

## Building

### Native build of `libghostty-vt`

`build.rs` drives the native build of `vendor/libghostty-vt`:

- It shells out to `zig build` (override the binary with the `ZIG` env var) for
  the host target, then emits the link directives for the resulting static
  library.
- All Zig output (the install prefix and cache) lands under Cargo's `OUT_DIR`,
  never inside `vendor/`. This keeps the vendored source tree pristine, which
  `cargo package` verification requires.
- The build skips entirely on docs.rs (`DOCS_RS` is set), since rustdoc doesn't
  link the native library.

A few env vars tune the native build, chiefly for packaging:
`LIBGHOSTTY_VT_OPTIMIZE` (default `ReleaseFast`), `LIBGHOSTTY_VT_SIMD`,
`LIBGHOSTTY_VT_ZIG_SYSTEM_DIR`, and `ZIG`.

### `vendor/` patch discipline

`vendor/libghostty-vt` is a vendored snapshot of an upstream source tree, pinned
in `vendor/libghostty-vt.vendor.json`. Don't edit the vendored files by hand to
carry a fix. Instead:

- Record every intentional local change as a patch under
  `vendor/patches/libghostty-vt/`, and track it in
  `vendor/libghostty-vt.patches.md` with its status, the upstream PR or
  discussion, the vendored base commit, the affected files, the reason, the
  remove-when condition, and a verification command.
- Remove a patch only once the vendored source contains the upstream fix and the
  listed verification still passes.
- Re-vendoring is driven by `just build-libghostty-vt`
  (`scripts/build_vendored_libghostty_vt.sh`), which rebuilds the vendored
  source distribution.

A unit test guards this discipline — see `scripts.test_vendor_libghostty_vt` in
the maintenance suite below.

## Running an isolated dev runtime

zynk runs as a long-lived server with agents living inside it. A dev build must
never touch a running production runtime: not its socket, not its config, and
not its database. Isolate the dev runtime on two axes.

**1. Isolate the build target.** A separate `CARGO_TARGET_DIR` keeps a dev build
off any default target a watcher might already own:

```bash
export CARGO_TARGET_DIR=$PWD/target-dev
```

**2. Isolate the runtime state.** Point every runtime path at a throwaway
directory so the dev binary can't reach the production socket, config, or DB:

```bash
export ZYNK_DEV=$PWD/.dev-runtime
mkdir -p "$ZYNK_DEV"

export ZYNK_CONFIG_PATH="$ZYNK_DEV/config.toml"   # config.toml location
export ZYNK_SOCKET_PATH="$ZYNK_DEV/zynk.sock"     # server-side socket
export ZYNK_CLIENT_SOCKET_PATH="$ZYNK_DEV/zynk.sock"  # client dials the same socket
export ZYNK_HOME="$ZYNK_DEV"                       # state home
export ZYNK_SQLITE_HOME="$ZYNK_DEV"                # conversation DB home
```

With those exports in place, run the dev binary through Cargo:

```bash
cargo run --release --locked --
```

Confirm the runtime you're attached to before doing anything stateful:

```bash
cargo run --release --locked -- status
```

`zynk status` reports the active socket and server. Verify it points at the dev
paths above — not the production runtime — before you proceed. Use a distinct
`--session <name>` to keep dev sessions separate from any default session.

## Testing

zynk's tests are hermetic: each test spawns its own temp config and socket, so
`cargo nextest` and `just test` are safe to run directly without the isolation
exports above.

- **Full test run:**

  ```bash
  just test
  ```

  This runs `cargo nextest run --locked` plus the maintenance-script unit tests
  (Python `unittest`) that guard agent-detection manifests, the vendored
  `libghostty-vt`, conventional commits, the public-tree / scrub / gitleaks
  content gates (including the tracked-tree scan), the skills catalog, and the
  release-audit references.

- **One test by filter:**

  ```bash
  just test-one <filter>      # e.g. just test-one codex_stale_working
  ```

- **TypeScript asset test (Bun):**

  ```bash
  just test-ts
  ```

## Lint and gates

- **Lint** — formatting and clippy:

  ```bash
  just lint    # cargo fmt --check + cargo clippy --all-targets --locked -- -D warnings
  ```

- **Private-content gates** — the structural and content checks that keep
  maintainer-private paths and strings out of the tree:

  ```bash
  just gate    # check_public_tree.py + scrub_check.py + gitleaks
  ```

- **Full check** — run this before opening a pull request:

  ```bash
  just check   # lint + TypeScript asset test + nextest + maintenance unit tests
  ```

  `just ci` runs the same lint and test path that the CI workflow runs.

Rust conventions: no `unwrap()` in production code, use `tracing` for logging,
and keep OS-specific behavior in `src/platform/`.

## Architecture

For the big picture — how state is separated from runtime, why render is a pure
function of `AppState`, where the socket command layer lives, and how
evidence-based agent detection works — read [`CLAUDE.md`](./CLAUDE.md).

The deeper design law and the architecture-decision records live in
`docs/zynk/`. Accepted ADRs are binding; amend a decision with a new ADR rather
than rewriting an old one.
