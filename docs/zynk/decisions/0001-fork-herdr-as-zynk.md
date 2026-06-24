# ADR 0001 — Fork zynk as zynk: AGPL, minimal rebrand, isolated dev runtime

**Status:** Proposed (gate: Codex review → operator). Foundational — other zynk ADRs build on it.
**Date:** 2026-06-10
**Spec:** `docs/zynk/SPEC.md` (gate-1 approved).

## Context

zynk was a zynk *wrapper* (frozen at `zynk` v1.5.1, MIT). The wrapper's entire delivery
machinery — marker verification, `classify_input`, `--source visible` scraping, marker-poll,
preflight (ADR 024–041 of the wrapper) — exists only because the wrapper observes zynk from
OUTSIDE and cannot trust its own state. zynk-native just submits and knows it.

The durable fix is to OWN the terminal layer. zynk (`dzevs/zynk`, Rust, single crate,
~153K LOC) is **AGPL-3.0-or-later** (dual with commercial), actively developed (5.1k★, 809 commits),
agent-aware (integrations for claude/codex/pi/omp/…), with a socket API that already exposes the
primitives zynk needs (`pane.send_input`, `report_agent`/`report_agent_session`/`report_metadata`,
`events.subscribe`). zynk has NO native conversation persistence — so zynk's value (protocol, audit,
DB, retrieval) is net-new.

Critical operational constraint: zynk + the frozen wrapper are LIVE on the dev machine and the
agents (claude/codex/pi — including the dev session itself) run INSIDE zynk. Dev/test must not
disrupt the live multiplexer.

## Decision

1. **zynk BECOMES a terminal workspace manager** (not a continued wrapper, not an upstream contribution, not a
   clean-room reimplementation). zynk owns the terminal layer; the wrapper's marker/scraping/
   classify/poll/preflight is **deleted, not ported**. "D8" (authoritative input-state) is not
   solved — it **evaporates** once the terminal is owned.
2. **License: AGPL-3.0-or-later, permanent.** zynk is a derivative of AGPL zynk; we do NOT own
   zynk's copyright, so the conveyed work must stay AGPL. No proprietary zynk without a future
   commercial license from the zynk author. zynk's copyright + LICENSE are preserved; `NOTICE`
   adds the zynk fork attribution (`© Zevs`). Own new modules MAY also be released as standalone
   MIT crates, but the shipped zynk binary is AGPL.
3. **Identity:** the fork IS `zynk`. crates.io `zynk` 0.x–1.5.1 stay MIT (frozen, immutable); the
   fork ships as a major bump **2.0.0 AGPL** with an explicit relicense note. One package, one
   version line. NO "zynk-terminal" / second package.
4. **Repo:** `git clone` zynk keeping history (oh-my-pi model) → `~/workspace/zynk`,
   remote `upstream` = `dzevs/zynk`, working branch `zynk-fork`. Eventual `origin` =
   `github.com/dzevs/zynk` as a **standalone repo** (not a GitHub fork-link; `git merge upstream`
   works regardless). **NO push / publish before full local test + operator gate** (hard rule).
5. **Rebrand: MINIMAL.** Rebrand brand/binary/docs/config/socket-path + `ZYNK_*` env (keep `ZYNK_*`
   compat during migration). KEEP internal module/API names close to upstream to minimize merge
   cost; avoid a global internal `zynk`→`zynk` rename. New zynk methods (`zynk.message_received`,
   …) are additive and clearly fork-owned.
6. **Runtime isolation for testing (enforceable, not prose; source-grounded).** Isolation is achieved
   by rebranding zynk's `app_dir_name()` → `zynk`/`zynk-dev`, so `config_dir()` and the data dir +
   BOTH sockets (which derive from it: `session.rs` `data_dir_for`/`active_api_socket_path`/
   `client_socket_path`) relocate to `~/.config/zynk[-dev]` by construction — never touching
   `~/.config/zynk`. (`ZYNK_CONFIG_PATH` is a config FILE, not a dir.) A **preflight MUST** print and
   assert `session_name`/`config_dir`/`config_path` (config FILE)/`api_socket`/`client_socket`/
   `target_dir` and ABORT (nonzero) if ANY resolves to the live zynk default (catching
   `ZYNK_SOCKET_PATH`/`ZYNK_CLIENT_SOCKET_PATH`/`ZYNK_CONFIG_PATH` overrides) or if
   `CARGO_TARGET_DIR` is unset/default. Dev runs scrub those override vars. The
   `db_path` assertion joins the preflight in M2 (when the DB exists). Broad `ZYNK_*` explicit-override
   env aliasing is a later, complete rebrand task (not required for isolation). No test runs against an
   un-asserted runtime.

## Alternatives considered

- **Contribute D8/features upstream to zynk** — keeps zynk a thin MIT wrapper, zero fork-maintenance,
  benefits the ecosystem; BUT gives zynk no ownership of the layer and is gated on the maintainer
  accepting the design. Rejected: the operator wants zynk to own the terminal layer.
- **Reimplement the terminal layer clean-room** (own code, MIT/proprietary possible) — massive
  (reinventing a 5.1k-star, 153K-LOC multiplexer). Rejected: disproportionate.
- **Buy a commercial license** (permissive/proprietary zynk possible) — costs money; deferred as the
  only path IF a proprietary zynk is ever required.
- **Stay a wrapper** — the marker/scraping bug class persists structurally. Rejected.

## Consequences

- zynk is **AGPL forever** (no proprietary without a commercial license). Accepted by the operator.
- **Perpetual upstream-merge burden** against an active project — mitigated by minimal rebrand,
  new `zynk_*` modules, additive hooks, and an explicit fork-patch ledger (`docs/zynk/decisions/`
  + `docs/zynk/fork-patch-ledger.md`).
- The wrapper's ADR 024–041 do NOT carry; they bind only frozen `zynk` v1.5.1. The fork writes its
  own ADRs under `docs/zynk/decisions/`.
- zynk attribution + AGPL must be preserved in all conveyed/network-served builds (`NOTICE`).
- Dev safety: the live zynk the agents run inside is never touched, by enforceable preflight.
