# agent-scripts

Agent scripts: saved agent calls as Markdown with frontmatter, and the runner that executes them, as a command line tool and a Rust library.

An agent script is a prompt with a harness, a model, the tools the agent may use, inputs and an output target, kept as one Markdown file. The runner validates it and runs it headless through the harness CLI (`claude`, `copilot`), from a terminal, a script, cron or CI, or embedded in an application through the library.

This repository holds the library crate `agent-scripts` and the binary `agent-scripts`. It parses, validates, lists and runs scripts (`validate`, `list`, `run`).

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

## Run

```sh
agent-scripts run summarize-file --file notes/a.md
agent-scripts run .agents/scripts/summarize-file.md --file notes/a.md --input audience=ops --json
```

`run <path|name>` takes a script file, or the name of a project or user script. An argument of lowercase letters, digits and `-` only is a name; write `./name` for such a file. The runner expands the prompt, starts the harness headless (print mode, no shell, no input) in the directory of the run and prints the answer.

| Flag                     | Value                                                                  |
| ------------------------ | ---------------------------------------------------------------------- |
| `--dir <d>`              | the working directory, `{dir}` (default: the current directory)       |
| `--input <k>=<v>`        | a declared input; may be repeated                                      |
| `--file <f>`             | `{file}`, passed as written                                            |
| `--selection <text>`     | `{selection}`                                                          |
| `--selection-file <f>`   | `{selection}`, read from a file                                        |
| `--json`                 | print the result record instead of the text                            |
| `--expect-sha256 <hash>` | refuse the script unless the SHA-256 of its content is this hex value |
| `--no-events`            | write no agent-events run (see [Events](#events))                     |

The record is `{ status, exitCode?, durationMs, text, changedFiles?, outputError? }`. `changedFiles` (in a git working tree) are the paths, relative to the repository root, that are modified or untracked at the end and whose content differs from the start; another process in the same tree shows there too. Changes inside submodules are not listed, and the key is absent when the run changed the git configuration (`git status` could start a program it names); the runner's own git calls fetch nothing.

| Exit code | Status      | Meaning                                                                         |
| --------- | ----------- | ------------------------------------------------------------------------------- |
| 0         | `ok`        | the harness succeeded and the output target was applied                         |
| 2         | `refused`   | usage error: unknown flag, missing required input (a bad flag prints no record) |
| 3         | `failed`    | the harness could not be started                                                |
| 4         | `error`     | the harness exited non-zero, or the output target failed (`outputError`)        |
| 5         | `timeout`   | the `timeout` of the script passed                                              |
| 6         | `refused`   | every other refusal: the harness was not started                                |
| 130, 143  | `cancelled` | SIGINT, SIGTERM (Windows: 130 for Ctrl-C and Ctrl-Break)                       |

SIGHUP and SIGQUIT cancel a run too (exit codes 129 and 131), unless they are ignored.

- **Tools:** the `tools` of the script are the only ones the agent has. The harness gets their names (the part before `(`) as the tools that exist and each entry as written as a tool that runs without a prompt. A model or tool entry that starts with `-` or names `bypassPermissions` is refused; a prompt that starts with `-` gets a line break before it.
- **Isolation:** the harness settings of the repository stay out (`claude`: `--setting-sources user --strict-mcp-config`); your own user settings apply. `copilot` reads `.github/copilot/settings.json` of the repository and has no flag to skip it, so its scripts are refused.
- **Timeout and cancel:** the process tree of the harness ends: SIGTERM, then SIGKILL after 5 s; on Windows a job object. What a harness leaves running after it ended is ended the same way. On Linux a process that cleared its environment, left the session of the harness and lost its parent within half a second is not found. On macOS a process that left the session of the harness and lost its parent within half a second is not found. A runner that is killed (`kill -9`) ends nothing on macOS; on Linux only the harness itself ends, not what it started (Windows ends everything). Such leftovers do not hold the lock of the script.
- **One run per script:** a second run of the same file is refused while one runs, also through another hard link to it. The file is the one on disk (device and inode; Windows: volume and file index), so a script that is replaced by a new file during a run, as some editors save, counts as another one.
- **Output:** `file` writes a new file and never overwrites; `append` appends. Both stay inside the directory of the run, also through symlinks, and write only after an `ok` run. `append` refuses a target with more than one hard link, wherever the other names are: the run ends `error` and nothing is written. `replace` is left to a host application; the command line refuses it.
- **Limits of the system:** the prompt is one argument of the harness, so the system may refuse a long one (Linux: 128 KiB; Windows: 32 767 characters for the whole command line). On Windows the harness must be an `.exe`; a `.cmd` file needs a shell.

The command line checks no trust: it runs the file you name. A host that starts the scripts of a repository passes `--expect-sha256`.

### Events

A run inside a git working tree writes one run in the [agent-events](https://github.com/GravionLabs/agent-events) format to `<git-common-dir>/.agents/events/<run>.jsonl`, which displays such as the Runs view of poimen read. It holds `run.started` (the script name as the title, the harness, the model, the process id), one task for the script, `task.finished` and `run.finished`; never the prompt or the answer. Outside a git working tree, or with `--no-events`, nothing is written.

| Status      | `outcome`   | `reason`    |
| ----------- | ----------- | ----------- |
| `ok`        | `succeeded` | `done`      |
| `error`     | `failed`    | `error`     |
| `failed`    | `failed`    | `failed`    |
| `timeout`   | `failed`    | `timeout`   |
| `refused`   | `failed`    | `refused`   |
| `cancelled` | `cancelled` | `cancelled` |

A run that is refused before the script is read (an unknown or invalid script, another SHA-256) has no events.

The environment variable `AGENT_EVENTS_DIR` (an absolute path) replaces the directory inside a git working tree; outside one nothing is written with it either. The runner deletes no old run files.

The environment variable `AGENT_EVENTS_RUN` sets the run id (`[A-Za-z0-9][A-Za-z0-9._-]{0,127}`). A run file is only ever created anew: with an id that is not valid or has a file already, a relative `AGENT_EVENTS_DIR`, and after a failed write, the run goes on and a warning goes to stderr.

### Harnesses

`claude` and `copilot` are built in. `config.json` in the user config directory (`~/.config/agent-scripts/` on Linux) replaces a definition by its id, as a whole:

```json
{
  "harnesses": {
    "claude": {
      "command": ["/opt/claude/claude"],
      "modelArgs": ["--model", "{model}"],
      "toolsArgs": ["--tools", "{tools}"],
      "allowToolArgs": ["--allowedTools", "{tool}"],
      "isolateArgs": ["--setting-sources", "user", "--strict-mcp-config"],
      "printArgs": ["-p", "{prompt}"]
    }
  }
}
```

A definition without `toolsArgs` or `isolateArgs` runs no script. `command` starts with a program name of the `PATH` or an absolute path. The environment variables `AGENT_SCRIPTS_CONFIG_DIR` and `AGENT_SCRIPTS_STATE_DIR` (absolute paths) replace the config directory and the directory of the run locks.

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
