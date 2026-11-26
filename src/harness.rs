//! Harness definitions (ADR 0019 "Harness call"): how an agent CLI is called in print mode with the
//! tools of a script only and without the settings of the repository. Templates expand per argv
//! element and never go through a shell.

use std::io;
use std::path::Path;

use serde_json::Value;

/// One harness. An empty list means that the harness has no such arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Harness {
    pub id: String,
    /// The program and its fixed arguments.
    pub command: Vec<String>,
    /// Added when the script sets a model; `{model}` is replaced inside an element.
    pub model_args: Vec<String>,
    /// The headless call; `{prompt}` is replaced inside an element.
    pub print_args: Vec<String>,
    /// The tools that exist; `{tools}` is replaced by the comma-joined tool names.
    pub tools_args: Vec<String>,
    /// Repeated per tool entry; `{tool}` is replaced by the entry as written.
    pub allow_tool_args: Vec<String>,
    /// Keep the harness settings of the repository out.
    pub isolate_args: Vec<String>,
}

/// What a script puts into the argv.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub model: Option<&'a str>,
    pub tools: &'a [String],
    pub prompt: &'a str,
}

/// Text no argv element of a script may carry, whatever the harness.
const BYPASS: &str = "bypassPermissions";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// The built-in definitions. `copilot` has no `isolate_args`: it reads
/// `.github/copilot/settings.json` of the repository and has no flag to skip it, so it runs no
/// script.
pub fn builtins() -> Vec<Harness> {
    vec![
        Harness {
            id: "claude".into(),
            command: strings(&["claude"]),
            model_args: strings(&["--model", "{model}"]),
            print_args: strings(&["-p", "{prompt}"]),
            tools_args: strings(&["--tools", "{tools}"]),
            allow_tool_args: strings(&["--allowedTools", "{tool}"]),
            isolate_args: strings(&["--setting-sources", "user", "--strict-mcp-config"]),
        },
        Harness {
            id: "copilot".into(),
            command: strings(&["copilot"]),
            model_args: strings(&["--model", "{model}"]),
            print_args: strings(&["-p", "{prompt}"]),
            tools_args: strings(&["--available-tools", "{tools}"]),
            allow_tool_args: strings(&["--allow-tool", "{tool}"]),
            isolate_args: Vec::new(),
        },
    ]
}

/// The definition of `id`: the one of the host, else the one of the user config file `config`, else
/// the built-in. Each replaces the others as a whole. A config file that cannot be understood is an
/// error: it may have meant to replace the built-in.
pub fn find(id: &str, host: &[Harness], config: Option<&Path>) -> Result<Harness, String> {
    if let Some(h) = host.iter().find(|h| h.id == id) {
        return Ok(h.clone());
    }
    if let Some(file) = config
        && let Some(h) = from_config(id, file).map_err(|e| format!("{}: {e}", file.display()))?
    {
        return Ok(h);
    }
    builtins()
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(|| format!("unknown harness {id:?}"))
}

/// `harnesses.<id>` of the config file; `None` when the file or the entry does not exist.
fn from_config(id: &str, file: &Path) -> Result<Option<Harness>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot be read: {e}")),
    };
    let config: Value = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
    let config = config.as_object().ok_or("must be a JSON object")?;
    let Some(harnesses) = config.get("harnesses") else {
        return Ok(None);
    };
    let harnesses = harnesses.as_object().ok_or("harnesses must be an object")?;
    harnesses
        .get(id)
        .map(|entry| parse(id, entry).map_err(|e| format!("harnesses.{id}: {e}")))
        .transpose()
}

fn parse(id: &str, entry: &Value) -> Result<Harness, String> {
    let entry = entry.as_object().ok_or("must be an object")?;
    let list = |key: &str| -> Result<Vec<String>, String> {
        let Some(value) = entry.get(key) else {
            return Ok(Vec::new());
        };
        value
            .as_array()
            .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| format!("{key} must be an array of strings"))
    };
    Ok(Harness {
        id: id.to_owned(),
        command: list("command")?,
        model_args: list("modelArgs")?,
        print_args: list("printArgs")?,
        tools_args: list("toolsArgs")?,
        allow_tool_args: list("allowToolArgs")?,
        isolate_args: list("isolateArgs")?,
    })
}

/// The name of the tool of an entry: the part before its `(`.
fn tool_name(entry: &str) -> &str {
    entry.split_once('(').map_or(entry, |(name, _)| name)
}

/// A value of a script that becomes an argument of its own must not read as an option of the
/// harness, nor ask to bypass the permission checks.
fn checked<'a>(what: &str, value: &'a str) -> Result<&'a str, String> {
    if value.is_empty() || value.starts_with('-') || value.contains(BYPASS) {
        return Err(format!("{what} {value:?} is not allowed"));
    }
    Ok(value)
}

fn expand<'a>(
    template: &'a [String],
    placeholder: &'a str,
    value: &'a str,
) -> impl Iterator<Item = String> + 'a {
    template.iter().map(move |t| t.replace(placeholder, value))
}

fn takes(template: &[String], placeholder: &str) -> bool {
    template.iter().any(|a| a.contains(placeholder))
}

impl Harness {
    /// The argv of a call: `command`, `isolate_args`, `model_args` when a model is set,
    /// `tools_args`, `allow_tool_args` per tool entry, `print_args`. An error is the reason why the
    /// call is refused: the harness cannot be limited, or a value of the script is not allowed.
    pub fn argv(&self, call: Call<'_>) -> Result<Vec<String>, String> {
        let id = &self.id;
        if self.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(format!("the harness {id:?} has no command"));
        }
        if self.isolate_args.is_empty() {
            let why = if id == "copilot" {
                ": copilot reads .github/copilot/settings.json of the repository and has no flag to skip it"
            } else {
                ""
            };
            return Err(format!(
                "the harness {id:?} has no isolateArgs, so it cannot keep the settings of the repository out{why}"
            ));
        }
        if !takes(&self.tools_args, "{tools}") {
            return Err(format!(
                "the harness {id:?} has no toolsArgs with {{tools}}, so its tools cannot be limited"
            ));
        }
        if !takes(&self.print_args, "{prompt}") {
            return Err(format!(
                "the harness {id:?} has no printArgs with {{prompt}}"
            ));
        }

        let mut argv = self.command.clone();
        argv.extend(self.isolate_args.iter().cloned());
        if let Some(model) = call.model {
            let model = checked("the model", model)?;
            argv.extend(expand(&self.model_args, "{model}", model));
        }
        let mut names: Vec<&str> = Vec::new();
        for entry in call.tools {
            checked("the tool", entry)?;
            let name = checked("the tool name of the entry", tool_name(entry))
                .map_err(|_| format!("the tool {entry:?} is not allowed"))?;
            if !names.contains(&name) {
                names.push(name);
            }
        }
        argv.extend(expand(&self.tools_args, "{tools}", &names.join(",")));
        for entry in call.tools {
            argv.extend(expand(&self.allow_tool_args, "{tool}", entry));
        }
        // A prompt such as a Markdown list starts with `-`, which a harness reads as an option.
        let guarded;
        let prompt = if call.prompt.starts_with('-') || call.prompt == BYPASS {
            guarded = format!("\n{}", call.prompt);
            &guarded
        } else {
            call.prompt
        };
        argv.extend(expand(&self.print_args, "{prompt}", prompt));
        Ok(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Harness {
        builtins().remove(0)
    }

    fn argv(h: &Harness, model: Option<&str>, tools: &[&str], prompt: &str) -> Vec<String> {
        let tools = strings(tools);
        h.argv(Call {
            model,
            tools: &tools,
            prompt,
        })
        .unwrap()
    }

    #[test]
    fn the_argv_of_claude() {
        let got = argv(
            &claude(),
            Some("sonnet"),
            &[
                "Read",
                "Bash(git status)",
                "Bash(git log:*)",
                "Read",
                "Grep",
            ],
            "do {tools} $(x)\n'y'",
        );
        assert_eq!(
            got,
            [
                "claude",
                "--setting-sources",
                "user",
                "--strict-mcp-config",
                "--model",
                "sonnet",
                "--tools",
                "Read,Bash,Grep",
                "--allowedTools",
                "Read",
                "--allowedTools",
                "Bash(git status)",
                "--allowedTools",
                "Bash(git log:*)",
                "--allowedTools",
                "Read",
                "--allowedTools",
                "Grep",
                "-p",
                "do {tools} $(x)\n'y'",
            ]
        );
    }

    #[test]
    fn no_tools_and_no_model() {
        assert_eq!(
            argv(&claude(), None, &[], "p"),
            [
                "claude",
                "--setting-sources",
                "user",
                "--strict-mcp-config",
                "--tools",
                "",
                "-p",
                "p"
            ]
        );
    }

    #[test]
    fn the_argv_of_copilot_once_it_has_isolation() {
        let mut copilot = builtins().remove(1);
        copilot.isolate_args = strings(&["--isolated"]);
        assert_eq!(
            argv(&copilot, Some("auto"), &["shell(git:*)"], "p"),
            [
                "copilot",
                "--isolated",
                "--model",
                "auto",
                "--available-tools",
                "shell",
                "--allow-tool",
                "shell(git:*)",
                "-p",
                "p"
            ]
        );
    }

    fn refusal(h: &Harness, model: Option<&str>, tools: &[&str]) -> String {
        let tools = strings(tools);
        h.argv(Call {
            model,
            tools: &tools,
            prompt: "p",
        })
        .unwrap_err()
    }

    #[test]
    fn a_harness_that_cannot_be_limited_is_refused() {
        let copilot = builtins().remove(1);
        assert!(refusal(&copilot, None, &[]).contains(".github/copilot/settings.json"));
        for broken in [
            Harness {
                isolate_args: Vec::new(),
                ..claude()
            },
            Harness {
                tools_args: Vec::new(),
                ..claude()
            },
            Harness {
                tools_args: strings(&["--tools", "all"]),
                ..claude()
            },
            Harness {
                print_args: strings(&["-p"]),
                ..claude()
            },
            Harness {
                command: Vec::new(),
                ..claude()
            },
        ] {
            refusal(&broken, None, &[]);
        }
    }

    #[test]
    fn a_script_cannot_put_an_option_into_the_argv() {
        for flag in [
            "--dangerously-skip-permissions",
            "--allow-all",
            "--allow-all-tools",
            "bypassPermissions",
            "--permission-mode=bypassPermissions",
            "-x",
            "",
        ] {
            refusal(&claude(), Some(flag), &[]);
            refusal(&claude(), None, &["Read", flag]);
        }
        refusal(&claude(), None, &["(git status)"]);
        refusal(&claude(), None, &["-x(y)"]);
    }

    #[test]
    fn a_prompt_never_starts_like_an_option() {
        for prompt in [
            "--dangerously-skip-permissions",
            "--allow-all",
            "- a list",
            "bypassPermissions",
        ] {
            let got = argv(&claude(), None, &[], prompt);
            assert_eq!(got.last().unwrap(), &format!("\n{prompt}"));
        }
    }

    #[test]
    fn the_host_wins_over_the_config_and_the_config_over_a_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        assert_eq!(find("claude", &[], Some(&file)).unwrap(), claude());
        std::fs::write(
            &file,
            r#"{"harnesses": {"claude": {"command": ["mine"]}, "bad": {"command": "x"}}}"#,
        )
        .unwrap();
        let mine = find("claude", &[], Some(&file)).unwrap();
        assert_eq!(mine.command, ["mine"]);
        // replaced as a whole, not merged
        assert!(mine.isolate_args.is_empty());
        assert_eq!(find("copilot", &[], Some(&file)).unwrap(), builtins()[1]);
        let host = Harness {
            command: strings(&["hosts"]),
            ..claude()
        };
        assert_eq!(
            find("claude", std::slice::from_ref(&host), Some(&file)).unwrap(),
            host
        );
        assert!(
            find("bad", &[], Some(&file))
                .unwrap_err()
                .contains("command")
        );
        assert!(
            find("nope", &[], Some(&file))
                .unwrap_err()
                .contains("unknown harness")
        );
        std::fs::write(&file, "{").unwrap();
        assert!(
            find("claude", &[], Some(&file))
                .unwrap_err()
                .contains("not valid JSON")
        );
    }
}
