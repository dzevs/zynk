# Security policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** to **hi@zevs.gg**. Don't open a public issue
for security problems.

Include a description, reproduction steps, the affected version/commit, and the impact. You'll get an
acknowledgement as soon as possible and a fix or mitigation timeline once the report is triaged.

## Supported versions

Security fixes land on `main` first and ship in the **next release**. Only the **newest version in each
distribution channel** is supported: the current GitHub Release (binaries), the newest crates.io version
and the current Homebrew formula. Channels can differ — a source-only crates.io release may be newer than
the binaries — so check the [CHANGELOG](./CHANGELOG.md) for what each release contains and which
channels carry it. Earlier versions receive no backports: upgrade to the newest version in your channel,
or build from source / Nix to run `main` with unreleased fixes (see the [README](./README.md)). Security fixes
are guaranteed to ship for the required platform, Linux x86_64; optional platforms (macOS on Apple silicon,
Windows) receive them only in releases where their artifact was eligible, or through a source build that may or
may not succeed ([ADR 0012](docs/zynk/decisions/0012-platform-support-tiers.md)). This section is updated
whenever a release is cut.
