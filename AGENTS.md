# AGENTS.md

Guidance for humans and coding agents working in this repository. Generated text (docs, issues, commits, code) is written in US English.

## What this is

agent-scripts is the format and the runner of **agent scripts**: saved agent calls as Markdown files with YAML frontmatter, run headless through a harness CLI (`claude`, `copilot`). It is a Rust library crate `agent-scripts` and a binary `agent-scripts`, shared by poimen and Nixe. The decision is GravionLabs/poimen [ADR 0019](https://github.com/GravionLabs/poimen/blob/release/next/docs/adr/0019-agent-scripts-format-and-runner.md).

- `src/lib.rs`: the library (format, runner).
- `src/main.rs`: the CLI.
- `tests/`: integration tests of the binary.

## Rules

- **Tests stay read-only.** A test never writes outside its own temporary directory, never changes the user's config, and never touches the repository it runs in.
- **No real agent in tests.** A test never starts `claude`, `copilot` or any other harness, and never needs a login or the network. A test of the runner uses a fake harness (a small program or script the test builds) in place of the real CLI.
- Every change ships with tests; a bug fix with a test that fails without the fix.
- `cargo clippy` with `-D warnings` stays clean; no `#[allow]` without a comment saying why.
- The toolchain is pinned in `rust-toolchain.toml`; raise it together with `rust-version` in `Cargo.toml`.

## Verification

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

CI (`.github/workflows/ci.yml`) runs the same on Linux, macOS and Windows; its `ci` job is the one required check on `main` and `release/**` (`.github/rulesets/main.json`).

## CI and releases

- Workflows and actions of GravionLabs/ci are pinned to a commit SHA with the version as a comment (`@<sha> # v1.19.0`), never `@main`, a branch or a major tag; other actions are pinned to a SHA too. Renovate (`.github/renovate.json5`) updates the SHAs, the comments and the Cargo dependencies.
- Release: dispatch `release.yml` on `main` (Actions, Release, Run workflow). GitVersion (`GitVersion.yml`, Conventional Commits; below 1.0 a breaking change raises the minor version) sets the version, the archives of every target are built and smoke tested, then the version is written into `Cargo.toml` and `Cargo.lock`, git-cliff (`cliff.toml`) regenerates `CHANGELOG.md`, both are committed to `main` and tagged `v<version>`, and the release gets `agent-scripts-<version>-<target>.tar.gz` (`.zip` on Windows) for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-pc-windows-msvc`, plus `SHA256SUMS`.
- A release tags only the commit it built, plus its own version and changelog commit: the release job fails, naming both SHAs and writing nothing, when `origin/main` is no longer the dispatched commit; dispatch the release again. This also refuses a re-run of the job after the version commit was pushed, so finish such a half-done release by hand (tag, GitHub Release, assets from the workflow artifact).
- `dry-run: true` builds the archives and `SHA256SUMS` as a workflow artifact without commit, tag or release; it runs on any branch.
- The release pushes to `main` past the ruleset with the secret `GH_PAT` (a repository admin's token). Renovate runs with the GravionLabs CI app (`CI_APP_ID`, `CI_APP_PRIVATE_KEY`).
- Commits and pull request titles follow Conventional Commits; `chore`, `ci`, `style`, `test` and `build` stay out of the changelog.
