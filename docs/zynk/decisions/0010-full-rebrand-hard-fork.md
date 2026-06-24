# ADR 0010 — Full rebrand & hard fork (zynk owns its identity)

**Status:** Proposed (gate: operator). **Date:** 2026-06-15 · **Milestone:** M6-rebrand (full).
**Amends / supersedes-in-part:** ADR 0007 §Decision 1 (crate/package name stays `zynk`),
§Decision 3 category-2 "internal symbol survivability" carve-out, §Decision 5 (`ZYNK_*` transitional
compat), and the matching "minimal rebrand" fork conventions in `CLAUDE.md` / `AGENTS.md`. The accepted
text of ADR 0007 is **not rewritten**; this ADR records the supersession.
**Unchanged by this ADR:** ADR 0009 (visible header / no footer) — correct direction, kept.

## Context

ADR 0007 chose a **minimal** rebrand: binary → `zynk` and all user-facing surfaces → `zynk`, but the
Cargo package/crate name, internal symbols, and `ZYNK_*` env aliases stayed `zynk`. That minimalism
had **exactly one** justification, stated in ADR 0007 §Alternatives: preserving `git merge upstream`
survivability ("the fork's core constraint") — a broad internal rename would make every upstream merge
a rename-conflict.

The operator has now decided (2026-06-15, recorded here):

1. **zynk is a HARD FORK.** We will not merge future upstream `zynk` changes. The single constraint
   that bound ADR 0007 to minimalism **no longer exists**.
2. In an **agent-driven** development model the ~1386 internal `zynk` references are active **context
   poisoning** — they pollute every agent's model of the repo's identity and defeat the purpose of the
   rebrand. "crate stays zynk" / "`ZYNK_*` compat" are no longer acceptable as default product/runtime
   behavior.

Therefore: **product, source, and runtime must be Zynk-native — not "zynk with a zynk wrapper."**

## Decision

1. **Hard fork — stop tracking upstream.** No future `git merge upstream zynk`. The
   upstream-merge-survivability constraint is formally dropped. The `upstream` remote + git history are
   retained for **provenance/attribution only**, not active merging.

2. **Full internal rebrand.** The Cargo **package/crate name becomes `zynk`** (was `zynk`); internal
   types/structs/enums/fns/consts/modules/log-targets/string-IDs containing `zynk` are renamed
   zynk-native (e.g. `RemoteZynk` → `RemoteZynk`, `ToastZynkPosition` → `ToastZynkPosition`,
   `ZynkToastConfig` → `ZynkToastConfig`). This supersedes ADR 0007 §Decision 1 and the §Decision 3
   category-2 internal-symbol carve-out. (Crate rename is low-risk: binary-only crate, no `[lib]`, zero
   external `zynk::` imports, tests already use `CARGO_BIN_EXE_zynk`.)

3. **`ZYNK_*` is the sole normal runtime/env surface.** `ZYNK_*` compat aliases are **removed**
   (supersedes ADR 0007 §Decision 5). Every product/runtime env var (incl. the pane-injection set read by
   integrations) is `ZYNK_*`. A specific `ZYNK_*` var may be retained **only** with an explicit,
   operator-approved transitional justification; default is **drop**.

4. **Provenance/source labels → `zynk:<agent>`.** The `agent_session` `source` ids and event-bus
   channels (`zynk:claude` / `zynk:codex` / `zynk:pi` / … / `zynk:blocked`) become `zynk:<agent>`.
   Safe because the DB is disposable and the runtime is only-us; all matching code (detection,
   persistence, `who`/`whoami`) changes in lockstep.

5. **DB/schema identity is zynk-native.** `zynk`-named columns (`zynk_event_id`) → `zynk_*`. Editing
   migration `0001` in place changes its sqlx checksum; existing DBs are wiped at next init — **accepted**
   (DB disposable, only-us; same posture as the `footer_json`→`protocol_json` rename).

6. **Allowed `zynk` — the ONLY surviving references.** (a) **AGPL legal attribution** —
   `NOTICE`, `LICENSE`, and source license headers crediting `ogulcancelik` + the zynk contributors —
   **legally mandatory, never removed**; (b) **upstream provenance** prose + the upstream repo URL as
   attribution; (c) **historical records that must not be rewritten** — accepted ADRs 0001–0009, the
   append-only `fork-patch-ledger.md`, dated `plans/*.md`. Everything runtime/product/source is `zynk`.

7. **Acceptance gate.** A CI grep/test asserts **zero prohibited `zynk`** in active source/docs
   (allowlisting only the legal/provenance/history set of §6); `ZYNK_*` is the sole normal env; source
   labels are `zynk:<agent>`; the crate/package name is `zynk`.

8. **M8-gated infra deferral.** Release-asset filenames (`zynk-{target}`) and the Windows install layout
   (`Programs/Zynk/bin/zynk.exe`, `ZYNK_INSTALL_DIR`) become `zynk` when the M8 release/installer
   infra lands; they are currently fail-closed behind the updater gate. Tracked, not forgotten.

9. **Commit ordering.** The ADR 0009 visible-header/no-footer work is correct and unchanged but is
   **not committed until this full-rebrand gate is addressed** (operator directive). No live install /
   dogfood / commit / push until the operator gates.

## Alternatives considered

- **Keep ADR 0007 minimal rebrand** — rejected by operator: leaves product identity as `zynk`,
  context-poisons agent-driven development, and defeats the rebrand's purpose.
- **Full rebrand while still tracking upstream** — rejected: would make every `git merge upstream` a
  rename-conflict; only viable *because* we chose a hard fork.
- **Drop `ZYNK_*` but keep internal symbols `zynk`** — rejected: still ships a zynk-named crate +
  internal identity; half-measure that does not satisfy "Zynk-native source".

## Consequences

- A large but **tractable** repo-source-only milestone (crate rename + internal symbols + ~42 env vars +
  ~280 source labels + a migration column), executed with `cargo build` + the full nextest suite as the
  oracle under an isolated `CARGO_TARGET_DIR`. See the full-rebrand audit report + implementation plan.
- `Cargo.lock` regenerated (package `zynk`→`zynk`); DB wiped at next init (migration checksum change);
  integrations reinstalled at the operator's later cutover (M7) so panes emit `zynk:<agent>`.
- **AGPL is preserved** — `NOTICE`/`LICENSE`/headers crediting zynk stay; no relicense, no attribution
  removal. The fork remains AGPL-3.0-or-later.
- The fork is no longer upstream-merge-survivable **by design** — an accepted trade of merge-tracking for
  a clean native identity.
