# Contributing

LazyTerm is early-stage. Small, focused contributions are easiest to review and least likely to conflict with architectural decisions.

## Before starting

- Search existing issues and pull requests for related work.
- For a feature or architectural change, open an issue first and agree on its scope.
- Bug fixes and documentation corrections can be proposed directly.
- Do not include credentials, private hostnames, private keys, or personal SSH configuration in examples or tests.

## Local checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Tests should not require a real SSH server, a real Keychain, or a developer's personal `~/.ssh/config`. Use temporary fixtures and fake adapters.

## Branch and pull request workflow

For changes that benefit from review or CI:

1. Create a focused branch from `main`, for example `feat/host-search` or `fix/restore-terminal`.
2. Make ordinary commits on that branch. Commit messages should describe the change.
3. Open a pull request against `main`, linking the issue and stating scope, behavior, and verification.
4. Wait for CI, inspect the diff, and address review findings.
5. Squash-merge the pull request and delete the branch.

The maintainer may review and merge their own pull request. A PR is useful even for a one-person project because it runs the same checks, records intent and discussion, and keeps `main` changes reviewable. It is a workflow choice, not a requirement for every commit. Tiny documentation fixes may go directly to `main` while the project is maintained by one person.

Keep pull requests narrow. Do not combine unrelated features or bump the application version in ordinary feature PRs.

## Versioning and releases

Commits and pull requests are development history; neither is itself a release. A release is a chosen, tested commit marked with a version tag and published with release notes and installable artifacts.

Before `1.0.0`, LazyTerm uses `0.y.z`:

- `0.y.(z+1)` for compatible bug fixes and small maintenance releases;
- `0.(y+1).0` for a feature milestone or a change that may require user migration;
- prereleases such as `0.2.0-alpha.1` for testing a planned milestone.

Once the core user-facing behavior and migration policy are stable, use `1.0.0`. After `1.0.0`, follow SemVer: patch for backward-compatible fixes, minor for backward-compatible functionality, major for incompatible changes.

For a release:

1. Confirm the matching roadmap gate and CI pass.
2. Update `Cargo.toml` and `Cargo.lock` in a release-preparation PR.
3. Add user-facing release notes, including known limitations and migration notes.
4. Tag the merged commit as `vX.Y.Z`.
5. Publish a GitHub Release and attach verified binaries/checksums when available.

Do not bump `Cargo.toml` for every PR. Cargo package version and GitHub release tag should agree when a release is published.

## Security issues

Do not publish credentials, private keys, or exploitable security details in a public issue. Contact the maintainer privately through the repository's GitHub security reporting channel when available.
