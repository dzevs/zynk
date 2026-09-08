# ADR 0012 — Platform support tiers: Linux x86_64 is required, everything else ships only on its own evidence

- **Status:** Accepted (release 3.1.0 pre-tag; operator decision 2026-09-08 relayed in the Gate-2 R13
  conversation, Codex Gate-1 design approval `msg_11ad2ad15512406f` on the support-tiers proposal)
- **Amends:** nothing. Clarifies the release contract that `WORKFLOW.md` ("Release gates") and the README
  Install section describe; ADR 0010 (full fork) is unaffected.
- **Review date:** 2027-03-08 — a review of this tiering, not an automatic restoration of equal-tier
  obligations.

## Context

The team runs zynk on Fedora and Ubuntu on Intel/AMD hardware, and the priority for the next six months is
internal productivity. Before this decision every hosted platform job and every release artifact was an
implicit release blocker: the 3.1.0 candidate at 246a901 was held by a macOS static-archive alignment failure,
an aarch64 cross-link failure and a Nix crate-download failure while the Linux artifact itself was fine. The
cross-platform code stays, and the fixes for those failures stay once they pass their own review (the pre-tag
fixes at `dfe864b` were still under Codex Gate-2 review when this ADR was written); what changes is what a
release **requires** versus what it **may include**.

Two facts shape the evidence design. `zynk --version` prints only `zynk <version>` (`src/build_info.rs`,
`src/main.rs`): no commit is embedded and none is added, so commit provenance is CI **checkout** provenance
recorded by the producing job. And a GitHub Actions re-run re-executes only failed or downstream jobs, so a
manifest in run attempt 2 may legitimately consume an artifact a successful producer made in attempt 1.

## Decision

1. **Tiers.** `linux-x86_64` (`x86_64-unknown-linux-gnu`, glibc ≥ 2.30) is the **required** release target.
   `macos-aarch64`, `windows-x86_64`, `macos-x86_64` and `linux-aarch64` are **optional**: a release may include
   them, and a failure on any of them never holds the Linux release. A shared correctness or safety defect that
   affects Linux remains a blocker wherever it was found.
2. **Eligibility.** An optional artifact is included only when it is **ELIGIBLE** in the candidate-evidence run at
   the candidate SHA: its applicable test job passed, its build job produced exactly the expected archive, the
   producing job executed the actual packaged binary on its native runner and wrote an `EVIDENCE.json` sidecar,
   and the manifest verified that sidecar's binding to the downloaded archive (archive and binary hashes,
   `GITHUB_SHA` and checked-out HEAD, requested version = `Cargo.toml` version = executed `--version` output,
   target/CPU from the binary header, producer run id, and the published ABI contract — for Linux the glibc
   floor ≤ 2.30). Every other outcome is `BUILT_UNVERIFIED`, `OMITTED (reason)` or `INCONSISTENT (reason)` and is
   **not published**. "Present" never authorizes publication; ELIGIBLE is evidence for the G2/operator inclusion
   decision, not permission to publish.
3. **Applicable test evidence** per target is fixed here: Linux x86_64 — `just check` on Ubuntu (nextest,
   maintenance unittests, TS); macOS Apple silicon — nextest without the `live_handoff` binary on the
   Apple-silicon runner; Windows x86_64 — the `windows_*` and client-transport unit tests, a build and the
   ConPTY smoke (a subset, named honestly). `macos-x86_64` and `linux-aarch64` have **no hosted test job**: a
   Rosetta or QEMU execution of the binary is execution evidence, not a substitute for a native test job, so
   these targets are at most `BUILT_UNVERIFIED` and are **not shipped in 3.1.0**.
4. **Provenance.** Each producer records its own `GITHUB_RUN_ID`/`GITHUB_RUN_ATTEMPT`; the manifest downloads
   only by the immutable artifact id its `needs` context names, keeps each producer's real attempt, allows
   same-SHA reuse across attempts, and rejects an artifact from another run or another commit. Unreferenced or
   stray retained artifacts are never read. The release-archive sha256 (what `SHA256SUMS` publishes and users
   verify) is distinct from upload-artifact's outer artifact digest. The manifest validates structure before
   it decides anything: the artifact directory must hold exactly the expected archive and sidecar as regular
   files, the sidecar must match the typed evidence schema (producer job, attempt, archive name, member,
   digests), the artifact id must be a single numeric id, and the download step's outcome must be `success` —
   any gap is INCONSISTENT, files from a failed or partial download are never read, and a decoding error on
   one target never aborts the manifest. The Linux glibc floor is measured from the binary's `.gnu.version_r`
   version needs (not from strings in the file) and must agree with the producer's native `objdump -T`
   evidence, which the producer must record; a binary that is not glibc-dynamic, has no version needs, or
   lacks that native evidence is INCONSISTENT. Download-outcome evidence is mandatory too: the manifest
   refuses anything but a typed outcome object and reads no artifact without a recorded `success`. Artifact
   ids are bounded to JavaScript's safe-integer range shared by the exporter and the consumer. Each download
   step carries its own timeout, and the manifest job's timeout leaves a documented reserve after the
   worst-case download total, so a slow optional download cannot cancel the job before the manifest exists.
5. **Required checks** (a failure stops the release; never bypassed): conventional commits; the private-content
   gates; `check-required` (CI, `just check` on Ubuntu); the Nix flake check **including** its `--all-systems
   --no-build` evaluation — a transitional exception: an evaluation failure on a non-Linux system would still
   block, and splitting that gate is a separate operator decision; in the candidate run `test-linux`,
   `build-linux-x86_64` and `manifest`; Codex Gate-2 and swarm Gate-3 on exact SHAs; every operator gate
   (ff-only merge, push, tag, release, crates.io, Homebrew — each separate). **Informational checks** (optional
   tier) never block, are never hidden or bypassed, and decide optional inclusion through the manifest.
6. **Validation cadence.** Per-push CI runs only the required check; optional-tier tests and builds run in the
   candidate-evidence workflow on demand (`workflow_dispatch`, `optional_targets: none | eligible | all`). Job
   timeouts bound execution, not queueing: when an optional job is still queued after 30 minutes, the
   required-only dispatch is the escape. Fedora validation for this transition is the operator's dogfood of the
   **candidate artifact** (checksum, version, distro and the exercised session/send/receipt/recovery flows
   recorded in the gate); an installed live binary is not evidence for an uninstalled candidate.
7. **Channels.** GitHub Release: the required archive always, optional archives only when ELIGIBLE, and
   `RELEASE_MANIFEST.txt` naming every target's status. crates.io `--locked`: source for every platform; optional
   platforms **may** build and **may** receive fixes — no promise. Homebrew tap: Linux always; macOS **per
   architecture** — a stanza only for an ELIGIBLE architecture; an omitted architecture's stanza is removed for
   that release with a note naming the last version that shipped it (never a missing asset, never an older
   binary presented as the new version); the tap bump keeps its own operator gate. Nix: `x86_64-linux` verified;
   other systems evaluated only.
8. **Retention.** Working cross-platform code and fixes are kept; nothing is deleted or reverted to become
   Linux-first. macOS/Windows-specific work is not an automatic priority until the review date; kept code
   promises no equal ongoing support.
9. **Enforcement.** `dzevs/zynk` has no branch protection and no rulesets (verified 2026-09-08), so "required" is
   enforced by the gate procedure reading the named jobs. If rulesets are ever introduced, the PR/push-required
   check names are `check-required`, `conventional-commits`, `gates` and the Nix `flake check`; the candidate
   workflow's `manifest` is dispatch-only release-gate evidence, never a PR check.

## Consequences

- 3.1.0 ships `linux-x86_64` and, when ELIGIBLE at the tag, `macos-aarch64` and `windows-x86_64`; it does not
  ship `macos-x86_64` or `linux-aarch64`. The README says so.
- Hosted Ubuntu now runs the maintenance unittests (`just check`), which `just ci` never did.
- `scripts/release_evidence.py` (producer) and `scripts/release_manifest.py` (consumer) are the tracked evidence
  tools, covered by maintenance unittests that model each manifest outcome, including a producer from attempt 1
  feeding a manifest in attempt 2.
- Optional-tier regressions surface only in candidate runs; that is the accepted cost of the six-month focus.
