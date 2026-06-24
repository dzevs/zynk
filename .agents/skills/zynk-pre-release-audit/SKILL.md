---
name: zynk-pre-release-audit
description: Audit zynk release readiness by comparing commits since the last release tag against the root CHANGELOG.md and README.md, plus version/tag/Nix-hash consistency. Use when asked to run the repo's pre-release audit or to check that the changelog and docs cover what shipped before a zynk release.
---

# Zynk pre-release audit

Use this skill only inside the zynk repository — the public single repo, canonical branch `main`.

Read `references/pre-release-audit.md` and follow its workflow. It is the source of truth for:

- choosing the release base ref (the latest `vX.Y.Z` tag)
- inspecting first-parent history and merged PRs since that tag
- auditing the root `CHANGELOG.md` (the single changelog) against what shipped
- auditing the root `README.md` for user-facing changes (commands, config, integrations, install/version pins)
- checking version consistency (`Cargo.toml` ↔ tag ↔ `CHANGELOG.md` ↔ `README.md`) and the Nix `cargoHash`
- listing the issue references the release will close
- producing the final release-readiness report

This is a READ-ONLY audit by default. Do not edit files unless the user explicitly asks to apply fixes, and
never run a release, tag, publish, `cargo publish`, or push — those are explicit operator-gated steps (`WORKFLOW.md`).
