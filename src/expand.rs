//! The variables of a prompt (ADR 0019 "Inputs"): the built-ins `{file}`, `{selection}` and `{dir}`
//! and the declared inputs. `{{` and `}}` are literal braces. The expansion is one pass over the
//! template, so a value that contains `{x}` stays as it is.

use std::collections::BTreeMap;
use std::fmt;

use crate::format::Input;

/// The built-in variables; no input may have one of these names.
pub const BUILTINS: [&str; 3] = ["file", "selection", "dir"];
/// The most bytes of an expanded prompt.
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// What the host gives a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values {
    pub file: Option<String>,
    pub selection: Option<String>,
    pub dir: Option<String>,
    /// The inputs by name. Names the script does not declare are ignored.
    pub inputs: BTreeMap<String, String>,
}

/// Why a template cannot be expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// A required input has no value and no default.
    MissingInput(String),
    /// The template uses a built-in variable the host did not give.
    MissingBuiltin(String),
    /// A placeholder that is neither built-in nor declared.
    UnknownPlaceholder(String),
    /// The result is over [`MAX_PROMPT_BYTES`].
    TooLarge,
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInput(n) => write!(f, "missing required input {n:?}"),
            Self::MissingBuiltin(n) => write!(f, "no value for the variable {{{n}}}"),
            Self::UnknownPlaceholder(n) => write!(f, "unknown placeholder {{{n}}}"),
            Self::TooLarge => write!(
                f,
                "the expanded prompt is over {} KiB",
                MAX_PROMPT_BYTES / 1024
            ),
        }
    }
}

impl std::error::Error for ExpandError {}

#[derive(Debug, PartialEq, Eq)]
enum Part<'a> {
    Text(&'a str),
    Placeholder(&'a str),
}

/// Cuts a template into text and placeholders. A brace without a partner is text.
fn parts(template: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push(Part::Text(&rest[..at]));
        let brace = &rest[at..at + 1];
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix(brace) {
            out.push(Part::Text(brace));
            rest = tail;
        } else if brace == "{"
            && let Some(end) = after.find('}')
        {
            out.push(Part::Placeholder(&after[..end]));
            rest = &after[end + 1..];
        } else {
            out.push(Part::Text(brace));
            rest = after;
        }
    }
    out.push(Part::Text(rest));
    out
}

/// The names of the placeholders of `template`, in order.
pub fn placeholders(template: &str) -> Vec<&str> {
    parts(template)
        .into_iter()
        .filter_map(|p| match p {
            Part::Placeholder(name) => Some(name),
            Part::Text(_) => None,
        })
        .collect()
}

/// Fills `template` (the prompt of a script) with `values` for the declared `inputs`. A required
/// input must have a value or a default, whether the template uses it or not; a built-in variable
/// is needed only where it is used. An optional input without a default is empty.
pub fn expand(
    template: &str,
    inputs: &BTreeMap<String, Input>,
    values: &Values,
) -> Result<String, ExpandError> {
    let mut resolved = BTreeMap::new();
    for (name, input) in inputs {
        let value = match values.inputs.get(name).or(input.default.as_ref()) {
            Some(v) => v.as_str(),
            None if input.required => return Err(ExpandError::MissingInput(name.clone())),
            None => "",
        };
        resolved.insert(name.as_str(), value);
    }
    let mut out = String::new();
    for part in parts(template) {
        let text = match part {
            Part::Text(text) => text,
            Part::Placeholder(name) => {
                let builtin = match name {
                    "file" => Some(&values.file),
                    "selection" => Some(&values.selection),
                    "dir" => Some(&values.dir),
                    _ => None,
                };
                match builtin {
                    Some(value) => value
                        .as_deref()
                        .ok_or_else(|| ExpandError::MissingBuiltin(name.to_owned()))?,
                    None => resolved
                        .get(name)
                        .copied()
                        .ok_or_else(|| ExpandError::UnknownPlaceholder(name.to_owned()))?,
                }
            }
        };
        if text.len() > MAX_PROMPT_BYTES - out.len() {
            return Err(ExpandError::TooLarge);
        }
        out.push_str(text);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(default: Option<&str>, required: bool) -> Input {
        Input {
            description: None,
            default: default.map(str::to_owned),
            required,
            extra: Default::default(),
        }
    }

    fn declared(list: &[(&str, Input)]) -> BTreeMap<String, Input> {
        list.iter()
            .map(|(n, i)| (n.to_string(), i.clone()))
            .collect()
    }

    fn given(list: &[(&str, &str)]) -> Values {
        Values {
            inputs: list
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            ..Values::default()
        }
    }

    #[test]
    fn fills_builtins_and_inputs() {
        let values = Values {
            file: Some("a.md".into()),
            selection: Some("text".into()),
            dir: Some("/w".into()),
            ..given(&[("who", "you")])
        };
        let inputs = declared(&[("who", input(None, true))]);
        assert_eq!(
            expand("{file} {selection} {dir} {who} {who}", &inputs, &values),
            Ok("a.md text /w you you".into())
        );
    }

    #[test]
    fn doubled_braces_are_literal() {
        let none = declared(&[]);
        let values = Values {
            file: Some("f".into()),
            ..Values::default()
        };
        assert_eq!(
            expand("{{file}} {{{file}}} {{\"a\": 1}}", &none, &values),
            Ok("{file} {f} {\"a\": 1}".into())
        );
    }

    #[test]
    fn a_single_brace_is_text() {
        let none = declared(&[]);
        assert_eq!(
            expand("a } b { c", &none, &Values::default()),
            Ok("a } b { c".into())
        );
        assert_eq!(placeholders("a } b { c"), Vec::<&str>::new());
        assert_eq!(placeholders("{a} {{b}} {c d}"), ["a", "c d"]);
    }

    #[test]
    fn a_value_is_not_expanded_again() {
        let values = Values {
            file: Some("{dir} {{x}}".into()),
            ..given(&[("a", "{file}")])
        };
        let inputs = declared(&[("a", input(None, true))]);
        assert_eq!(
            expand("{a}|{file}", &inputs, &values),
            Ok("{file}|{dir} {{x}}".into())
        );
    }

    #[test]
    fn defaults_and_optional_inputs() {
        let inputs = declared(&[("d", input(Some("dev"), false)), ("o", input(None, false))]);
        assert_eq!(
            expand("[{d}][{o}]", &inputs, &Values::default()),
            Ok("[dev][]".into())
        );
        assert_eq!(
            expand("[{d}]", &inputs, &given(&[("d", "ops")])),
            Ok("[ops]".into())
        );
        // `required: true` with a default: the default is the value
        let inputs = declared(&[("d", input(Some("dev"), true))]);
        assert_eq!(expand("{d}", &inputs, &Values::default()), Ok("dev".into()));
    }

    #[test]
    fn a_missing_required_input_is_an_error_even_when_unused() {
        let inputs = declared(&[("who", input(None, true))]);
        assert_eq!(
            expand("hello", &inputs, &Values::default()),
            Err(ExpandError::MissingInput("who".into()))
        );
    }

    #[test]
    fn a_missing_builtin_is_an_error_only_when_used() {
        let none = declared(&[]);
        for name in BUILTINS {
            assert_eq!(
                expand(&format!("x {{{name}}}"), &none, &Values::default()),
                Err(ExpandError::MissingBuiltin(name.into()))
            );
        }
        assert_eq!(expand("x", &none, &Values::default()), Ok("x".into()));
    }

    #[test]
    fn an_unknown_placeholder_is_an_error() {
        // also when the host passes a value for the undeclared name
        assert_eq!(
            expand("{nope}", &declared(&[]), &given(&[("nope", "v")])),
            Err(ExpandError::UnknownPlaceholder("nope".into()))
        );
    }

    #[test]
    fn the_prompt_is_at_most_256_kib() {
        let none = declared(&[]);
        let values = |n: usize| Values {
            selection: Some("a".repeat(n)),
            ..Values::default()
        };
        let max = 256 * 1024;
        let full = expand("x{selection}", &none, &values(max - 1)).unwrap();
        assert_eq!(full.len(), max);
        assert_eq!(
            expand("x{selection}", &none, &values(max)),
            Err(ExpandError::TooLarge)
        );
        assert_eq!(
            expand("{selection}x", &none, &values(max)),
            Err(ExpandError::TooLarge)
        );
        assert_eq!(
            expand(&"y".repeat(max + 1), &none, &Values::default()),
            Err(ExpandError::TooLarge)
        );
    }
}
