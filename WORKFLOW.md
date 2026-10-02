# WORKFLOW.md — zynk development & review gates

Authoritative for how zynk is developed and released. Read this before any state-changing repo operation
(merge, push, tag, release, publish). It separates collaborative development from independent verification so
unverified agent output never reaches `main`. Project conventions live in `CLAUDE.md`; agent operating rules in
`AGENTS.md`.

## Roles

- **Operator** — starts tasks; gives the final explicit merge / push / release approval.
- **Codex** — the single implementer/executor. Only Codex edits files, commits, and runs approved
  merge/push/tag/release operations, and waits for an explicit operator gate per action.
- **Claude** — collaborative reviewer (tier confirmation / Gate-1 + Gate-2 implementation) and context owner,
  read-only on implementation. A co-author, not independent.
- **Swarm** — Standard/Major Gate-3 independent verification: a bounded fan-out of specialist reviewers,
  decorrelated from the author.
- **Pi** — coordinator / relay; READ-ONLY verification. Doesn't write code, commit, or push.
- **zynk** — the audited transport; the conversation is the verdict record.

Role changes are operator-ordered and recorded. Prior approvals stay valid only for their recorded ranges under
the original reviewers; transferring implementation ownership never approves inherited commits.

## Binding rules

1. No merge / push / tag / release / publish until the operator explicitly approves — each is a separate gate.
2. No "ready to merge" claim until every gate required by the confirmed tier approves.
3. Pi must not edit source files or write code; Pi coordinates and verifies read-only.
4. On any gate/check failure: STOP and follow **Stops and cost ceiling** below. Never bypass (`--no-verify` is
   forbidden). Every check is required — zynk builds for one target (ADR 0013), so there is no informational tier.
5. Local builds/tests use an isolated `CARGO_TARGET_DIR`, never the live runtime.
6. The host is shared. Agents coordinate only inside their own project workspace; never pause, schedule around,
   or request pauses from another project's agents. A Major-tier CPU measurement step enforces its own quiet-host
   check by observing load and retrying later; it never asks other agents to stop.
7. Precise `git add <path>` — never `git add -A`. Lowercase conventional commits; never force-push `main`.
8. Freeze the candidate branch and worktree from Gate-2 submission until the tier's required review verdicts.
   No candidate edits, new commits, or teammate fixes while that review is open; reviewer probes belong in
   isolated copies. After the verdict, address the collected required findings in one bounded batch.
9. Review the successor delta and affected invariants. Nonblocking notes do not reopen accepted,
   unchanged areas; reopening requires concrete regression evidence. No fresh broad review fan-out
   while corrections to its parent are in progress. Separate approved milestones require separate worktrees.
10. Unreviewed fixes are **IMPLEMENTED / PENDING VERIFICATION**, never closed merely because a
   commit landed or the author's tests passed. Record approval only for the exact reviewed range.

## Upstream port policy

- FORK-OWNED user-visible behavior wins by default in any upstream port. This includes UI, glyphs, animation,
  layout, interaction, config effect, integration behavior, and CLI/agent surfaces.
- Every user-visible removal, replacement, or behavior change, whether fork-owned or upstream-origin, requires
  an explicit operator decision at Gate-1.
- A ledger entry alone is never sufficient evidence or approval for a user-visible change.

## Tiers

| tier | scope |
|---|---|
| **Light** | Docs, test hardening, version bump/tag/publish, and bounded bug fixes (about 200 changed production lines or fewer) with no contract change. |
| **Standard** | Product features and fixes that affect UI, config effect, integrations, or bounded runtime behavior. |
| **Major** | Upstream ports; wire/API/DB/snapshot schema changes; identity/security changes; or a new release line. |

Codex states the tier and reason in the kickoff/proposal, including paths touched and contracts changed. Claude
confirms or raises the tier in the first reply; for Light, that reply is the Gate-1 equivalent. When in doubt,
use the heavier tier. The operator may override the classification. Upgrade automatically mid-task if the change
touches wire/API/DB/snapshot schema, identity/security, or exceeds the Light bound; never downgrade mid-task.

## Gates per tier

| tier | required gates |
|---|---|
| **Light** | Local `just check` + `just gate` in the isolated target; Claude Gate-2 diff read; operator merge/push gate; hosted CI after push. No Gate-1 document and no Gate-3. |
| **Standard** | Short Gate-1 proposal (at most one page: goal, paths, risks, evidence to be produced); Gate-2; one read-only Gate-3 swarm with an arbiter plus at most three lanes, one pass, and one consolidated verdict; operator gate. No sealed packet. |
| **Major** | Full proposal plus a frozen numbered acceptance specification per program; Gate-2; Gate-3 swarm against that specification; operator gate. |

Gate-2 is Claude's collaborative diff review of the exact implementation. Gate-3 is independent and
decorrelated from the author; its authoritative consolidated verdict is the audited zynk conversation
(`zynk thread` / `zynk trace <id>`), not transport submission status.

## Evidence per tier

| tier | required evidence |
|---|---|
| **Light** | `just check`, `just gate`, hosted CI, and a plain-text record of commands, exits, and counts. |
| **Standard** | Light evidence plus red-first TDD logs. Run `just release-audit` only when update/release code is touched. Produce a fault matrix or CPU evidence only when the proposal names that performance/regression risk. |
| **Major** | Frozen acceptance specification; layer A exact records bound to the SHA (full test run, check, lint, gate, release audit, fault matrix, CPU rows); layer B strict manifest, canonical tree hash, and signed external seal. Layer C (reviewer bootstrap, rehearsal twin, STOPS/boundary/lifecycle indexes, machinery classifier) runs only when the operator opts in explicitly in the specification; it is off by default. |

Any Gate-3 blocker, in any tier where Gate-3 runs, must name a concrete product or public-contract defect with
`file:line`. Evidence packaging is never a blocker.

## Stops and cost ceiling

- **Author-tooling or hygiene stops** (wrappers, caches, quoting, parser assumptions) never return to Gate-1.
  Retain the failed run, fix the tool, record it, and report it in the next status.
- **Evidentiary failures** (full run, CI, fault, CPU) get one disposition from Claude and at most one fresh run.
  A second failure goes to the operator.
- **Documented intermittent tests** allow one rerun and must be hardened in the next Light change.
- **Cost ceiling:** when one review loop exceeds three stops or about four hours of implementer time on
  non-product work, Claude stops and gives the operator a cost/benefit table with a scope-cut option instead of
  continuing.

## Merge rule

Codex may report "ready to merge" only when all tier-required gates approve, Codex has checked every cited
`file:line`, and the operator explicitly approves. Merge to `main` is a fast-forward; pushes are operator-gated;
never force-push. Claude never executes mutating repository or system actions (merge, push, cleanup, deletes,
installs); Codex executes them only on an operator gate, and Claude verifies.

## Private content gate

Two-plus fail-closed layers run in pre-commit (`just install-hooks`) AND CI (`.github/workflows/gates.yml`):

- **Structural** `scripts/check_public_tree.py` — fails if a forbidden/private path is tracked.
- **Scrub** `scripts/scrub_check.py` — fails on product-specific reference terms in copied tooling/docs.
- **Content** `gitleaks` (`.gitleaks.toml`) — fails on private strings.

Run whole-tree with `just gate`. On any failure: STOP, fix the root cause, never bypass.

## Release gates (each a separate operator gate)

A version bump commit, signed `vX.Y.Z` tag, crates.io publish (`cargo publish`), and GitHub release each need a
separate explicit operator approval. Never tag / release / publish / yank / bump-version / force-push without
one. Released tags are immutable provenance anchors. zynk builds for Linux x86_64 only (ADR 0013): one target,
no release artifacts to verify.

The proven order is bump commit → hosted CI green → signed tag → crates.io publish → GitHub release
(source-only). Each step is an operator gate. No reviewer machinery runs between these release gates.

**Release gates** (a failure stops the release, never bypassed): `just check` — the same path as CI
`check-required` (`just check` on Ubuntu); `just gate` (the private-content gates, `gates.yml`); conventional
commits; tier-required review on exact SHAs; and the operator merge/push gate. Fedora validation is the
operator's dogfood of a binary built locally from the exact reviewed SHA, recorded in the gate with that SHA,
the binary's sha256, the version and the exercised session/send/receipt/recovery flows; the installed live
binary isn't evidence for an uninstalled candidate.

Enforcement: `dzevs/zynk` has no branch protection or rulesets, so "required" is enforced by this procedure
reading the named jobs. If rulesets are ever introduced, the PR/push-required check names are `check-required`,
`conventional-commits` and `gates`.

## Message bodies (native zynk)

```text
Request review : zynk send <pane> --type request-review  --trace <id> -- "<spec/diff + scope + risk labels>"
Approve        : zynk send <pane> --type approve          --trace <id> -- "APPROVE. <evidence: report/diff + file:line>"
Request changes: zynk send <pane> --type request-changes  --trace <id> -- "REQUEST_CHANGES. <specifics>"
```

## Recorded operator-gate deviation (2026-09-13)

Codex pushed `port/linux-only-v0.8.2` at the M7-reviewed `f821af47` after
interpreting the operator's repeated approval to finish the remaining port as
also covering push. The retained instructions do not separately name push, so
this was inferred authorization, not the explicit action gate required above.
The deviation was surfaced directly to the operator. A reviewer accepting this
record does not close it on the operator's behalf. `main`, tags, the installed
binary and release state did not change. No rewind, remote deletion or force
push is implied as a remedy. This note records the event without relaxing any
gate; the matching append is in `docs/zynk/fork-patch-ledger.md`.
