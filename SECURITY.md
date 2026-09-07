# Security policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** to **hi@zevs.gg**. Don't open a public issue
for security problems.

Include a description, reproduction steps, the affected version/commit, and the impact. You'll get an
acknowledgement as soon as possible and a fix or mitigation timeline once the report is triaged.

## Supported versions

Security fixes land on `main` first and ship in the **next release**. Only the **latest published
release** is supported — the current GitHub Release (binaries), the matching crates.io version and the
Homebrew formula; see the [CHANGELOG](./CHANGELOG.md) for what each release contains. Earlier releases
receive no backports: upgrade to the latest release, or build from source / Nix to run `main` with
unreleased fixes (see the [README](./README.md)). This section is updated whenever a release is cut.
