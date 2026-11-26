# agent-scripts

Agent scripts: saved agent calls as Markdown with frontmatter, and the runner (Rust crate and CLI) shared by poimen and Nixe.

An agent script is a prompt with a harness, a model, the tools the agent may use, inputs and an output target, kept as one Markdown file. The runner validates it and runs it headless through the harness CLI (`claude`, `copilot`). The format and the runner are decided in GravionLabs/poimen [ADR 0019: Agent scripts: format and runner](https://github.com/GravionLabs/poimen/blob/release/next/docs/adr/0019-agent-scripts-format-and-runner.md).

This repository holds the library crate `agent-scripts` and the binary `agent-scripts`. Parsing, running and the subcommands (`run`, `list`, `validate`) are not built yet; the binary has `--version` and `--help`.

## Install

Download the archive for your platform from the [releases](https://github.com/GravionLabs/agent-scripts/releases) (`agent-scripts-<version>-<target>.tar.gz`, `.zip` on Windows), check it against `SHA256SUMS` and put `agent-scripts` on your `PATH`:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

Or build it from source with the toolchain of `rust-toolchain.toml`:

```sh
cargo build --release
./target/release/agent-scripts --version
```

## Development

See [AGENTS.md](AGENTS.md) for the rules and the verify commands.

## License

MIT, see [LICENSE](LICENSE).
