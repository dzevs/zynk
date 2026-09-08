# Security policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** to **hi@zevs.gg**. Don't open a public issue
for security problems.

Include a description, reproduction steps, the affected version/commit, and the impact. You'll get an
acknowledgement as soon as possible and a fix or mitigation timeline once the report is triaged.

## Supported versions

Security fixes land on `main` first and ship in the **next release**. zynk is distributed as source only:
the supported channel is a build from this repository, or the newest crates.io version. Earlier versions
receive no backports — upgrade to the newest crates.io version, or build `main` from source to run with
unreleased fixes (see the [README](./README.md)). The supported platform is **Linux x86_64**; zynk doesn't
build for any other target ([ADR 0013](docs/zynk/decisions/0013-linux-only-platform-scope.md)). Check the
[CHANGELOG](./CHANGELOG.md) for what each release contains. This section is updated whenever a release is cut.
