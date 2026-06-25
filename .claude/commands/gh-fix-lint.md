# Fix Lint for a PR

Fix `cargo fmt` / `cargo clippy` failures for a PR branch.

1. Check out the PR branch (`gh pr checkout <number-or-url>`).
2. Run `just lint` (= `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings`) with an
   isolated `CARGO_TARGET_DIR`. Apply `cargo fmt` and fix clippy warnings (dead code fails the gate — use a
   justified `#[allow]` only when genuinely warranted).
3. Re-run `just lint` until clean, then commit gate-safe (`/commit`).

**Pushing is an operator gate** (`WORKFLOW.md`) — do not push without explicit approval.
