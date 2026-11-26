# agent-scripts

Agent scripts: saved agent calls as Markdown with frontmatter, and the runner (Rust crate and CLI) shared by poimen and Nixe.

An agent script is a prompt with a harness, a model, the tools the agent may use, inputs and an output target, kept as one Markdown file. The runner validates it and runs it headless through the harness CLI (`claude`, `copilot`). The format and the runner are decided in GravionLabs/poimen [ADR 0019: Agent scripts: format and runner](https://github.com/GravionLabs/poimen/blob/release/next/docs/adr/0019-agent-scripts-format-and-runner.md).

This repository holds the library crate `agent-scripts` and the binary `agent-scripts`. It parses, validates and lists scripts (`validate`, `list`); running them (`run`) is not built yet.

## Format

One Markdown file per script: YAML frontmatter between `---` lines, then the prompt as the body. This is `summarize-file.md`:

```markdown
---
name: summarize-file
description: Summary of a file in five bullet points
harness: claude
model: sonnet
tools: [Read]
inputs:
  audience:
    description: Who reads the summary
    default: developers
output:
  target: show
timeout: 5m
---

Summarize {file} for {audience} in five bullet points.
```

| Key           | Type                                                  | Default                     |
| ------------- | ----------------------------------------------------- | --------------------------- |
| `format`      | integer                                               | `1`                         |
| `name`        | `[a-z0-9][a-z0-9-]*`, at most 64 characters           | the file name without `.md` |
| `description` | string                                                | none                        |
| `harness`     | string, a harness id                                  | `claude`                    |
| `model`       | string, a model of the harness                        | the harness's default       |
| `tools`       | array of tool names or rules of the harness           | `[]` (no tools)             |
| `inputs`      | map of name to `{ description?, default?, required? }` | `{}`                       |
| `output`      | `target` (`show`, `file`, `append`, `replace`), `path` | `{ target: show }`         |
| `timeout`     | a number with `s`, `m` or `h`, at most `2h`           | `10m`                       |
| `triggers`    | array, reserved                                       | `[]`                        |

- The body may use `{file}`, `{selection}`, `{dir}` and the declared inputs; `{{` and `}}` are literal braces. An input name is `[a-z][a-z0-9_]*` and not a built-in variable; it is `required` unless it has a `default`. The expanded prompt is at most 256 KiB.
- `path` is required for `file` and `append` and stays inside `{dir}`.
- An unknown key gives a warning. An invalid value, missing or broken frontmatter, an empty body, an unknown `format`, an unknown placeholder or a `name` used twice in one directory makes the script invalid, and it never runs.

The JSON schema of the frontmatter is [`schema/agent-script.schema.json`](schema/agent-script.schema.json); the checks it cannot express (body, file name, directory, path, the `2h` limit) are the parser's.

## Use

```sh
agent-scripts validate .agents/scripts/summarize-file.md   # exit code 0, or 2 with the reasons
agent-scripts list [<dir>]                                 # name, source, harness, valid or the reason
```

`list` shows the project scripts (`.agents/scripts/*.md` in the root of the repository that holds `<dir>`, default the current directory) and the user scripts (`agent-scripts/scripts/*.md` in the user config directory: `~/.config` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows). A project script replaces the user script of the same name.

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
