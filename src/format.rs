//! The script file (ADR 0019 "Format", format 1): YAML frontmatter between `---` lines, then the
//! prompt. The schema of the frontmatter is `schema/agent-script.schema.json`; a test keeps the two
//! in step.
//!
//! Tolerance works per file: an unknown key is kept with a warning, any invalid value makes the
//! whole script invalid. Nothing falls back to a default, because a dropped `tools` or `timeout`
//! would change what the agent may do.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_yaml_ng::{Mapping, Value};

use crate::expand;

/// The only format this version reads.
pub const FORMAT: u32 = 1;
/// The harness of a script without `harness`.
pub const DEFAULT_HARNESS: &str = "claude";
/// The timeout of a script without `timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The longest `timeout` a script may set.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// The most characters of a `name`.
pub const MAX_NAME_LEN: usize = 64;

/// One problem of a script file. `key` is the frontmatter key (`output.path`), or `frontmatter`,
/// `body` or `file` for a problem that belongs to no key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub key: String,
    pub reason: String,
}

impl Issue {
    pub(crate) fn new(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Issue {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.reason)
    }
}

/// A declared input of a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub description: Option<String>,
    pub default: Option<String>,
    /// `true` without a `default`, else `false`, unless the script says otherwise.
    pub required: bool,
    /// Unknown keys, kept.
    pub extra: Mapping,
}

/// Where the answer of a run goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Show,
    File,
    Append,
    Replace,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    pub target: Target,
    /// Relative to `{dir}`, variables not yet expanded. Always set for `file` and `append`.
    pub path: Option<String>,
    /// Unknown keys, kept.
    pub extra: Mapping,
}

/// A valid script.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    /// The file it was read from.
    pub file: PathBuf,
    pub name: String,
    pub description: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub inputs: BTreeMap<String, Input>,
    pub output: Output,
    pub timeout: Duration,
    /// Reserved; kept as written.
    pub triggers: Vec<Value>,
    /// The body of the file, variables not yet expanded.
    pub prompt: String,
    /// Unknown top-level keys, kept.
    pub extra: Mapping,
    /// One entry per unknown key.
    pub warnings: Vec<Issue>,
}

/// A script that never runs, with every reason found.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub file: PathBuf,
    /// The `name` of the frontmatter when it is valid, else the file name without `.md`.
    pub name: String,
    /// Whether the file is known to carry `name`: it sets it, or its frontmatter was read and sets
    /// none. `false` when `name` is only a stand-in (broken frontmatter, an invalid `name`); such a
    /// name makes no other script a duplicate.
    pub named: bool,
    /// The harness, when it could be read.
    pub harness: Option<String>,
    /// Never empty.
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

impl Script {
    /// Turns the script into an invalid entry because of `error` (found outside the file's text).
    pub(crate) fn into_invalid(self, error: Issue) -> Invalid {
        Invalid {
            file: self.file,
            name: self.name,
            named: true,
            harness: Some(self.harness),
            errors: vec![error],
            warnings: self.warnings,
        }
    }
}

/// The file name without `.md`.
pub(crate) fn file_stem(file: &Path) -> String {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.strip_suffix(".md") {
        Some(stem) => stem.to_owned(),
        None => name,
    }
}

/// Splits `text` into the frontmatter and the body. The first line must be `---`, and the
/// frontmatter ends at the next line that is `---`.
pub fn split_frontmatter(text: &str) -> Result<(&str, &str), &'static str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = |line: &str| line.trim_end() == "---";
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().filter(|l| delimiter(l));
    let start = first
        .ok_or("missing: the file does not start with a `---` line")?
        .len();
    let mut end = start;
    for line in lines {
        if delimiter(line) {
            return Ok((&text[start..end], &text[end + line.len()..]));
        }
        end += line.len();
    }
    Err("not closed: there is no second `---` line")
}

/// Parses the text of the script file `file`. Reads nothing: the checks that need the other files
/// of the directory are in [`crate::dir`].
pub fn parse(file: &Path, text: &str) -> Result<Script, Box<Invalid>> {
    let stem = file_stem(file);
    let invalid = |key: &str, reason: String| {
        Box::new(Invalid {
            file: file.to_owned(),
            name: stem.clone(),
            named: false,
            harness: None,
            errors: vec![Issue::new(key, reason)],
            warnings: Vec::new(),
        })
    };

    let (front, body) =
        split_frontmatter(text).map_err(|reason| invalid("frontmatter", reason.into()))?;
    let map = match serde_yaml_ng::from_str::<Value>(front) {
        Ok(Value::Mapping(map)) => map,
        Ok(_) => {
            return Err(invalid(
                "frontmatter",
                "not a mapping of keys to values".into(),
            ));
        }
        Err(e) => return Err(invalid("frontmatter", format!("not valid YAML: {e}"))),
    };
    // The keys of an unknown format may mean something else, so nothing else is checked.
    if let Some(v) = map.get("format")
        && !matches!(v, Value::Number(n) if n.as_f64() == Some(f64::from(FORMAT)))
    {
        let mut unknown = invalid(
            "format",
            format!("unknown format {}; this version reads {FORMAT}", show(v)),
        );
        // The name still counts: the script keeps its place before the user script of that name.
        match map.get("name") {
            None => unknown.named = true,
            Some(Value::String(n)) if is_name(n) => {
                unknown.name = n.clone();
                unknown.named = true;
            }
            Some(_) => {}
        }
        return Err(unknown);
    }

    let mut p = Parser::default();
    let mut name = None;
    let mut description = None;
    let mut harness = Some(DEFAULT_HARNESS.to_owned());
    let mut model = None;
    let mut tools = Vec::new();
    let mut inputs = BTreeMap::new();
    // Names of all declared inputs, valid or not, so a broken input is not also reported as an
    // unknown placeholder.
    let mut declared = Vec::new();
    let mut output = Output::default();
    let mut timeout = DEFAULT_TIMEOUT;
    let mut triggers = Vec::new();
    let mut extra = Mapping::new();

    for (key, value) in &map {
        let Some(key) = key.as_str() else {
            p.error("frontmatter", format!("key {} is not a string", show(key)));
            continue;
        };
        match key {
            "format" => {}
            "name" => name = p.name(value),
            "description" => description = p.string("description", value),
            "harness" => harness = p.non_empty("harness", value),
            "model" => model = p.non_empty("model", value),
            "tools" => tools = p.tools(value),
            "inputs" => inputs = p.inputs(value, &mut declared),
            "output" => output = p.output(value),
            "timeout" => timeout = p.timeout(value).unwrap_or(timeout),
            "triggers" => match value {
                Value::Sequence(items) => triggers = items.clone(),
                _ => p.error("triggers", "must be an array"),
            },
            _ => p.unknown(&mut extra, key, key, value),
        }
    }

    if !map.contains_key("name") {
        if is_name(&stem) {
            name = Some(stem.clone());
        } else {
            p.error(
                "name",
                format!(
                    "the file name {stem:?} is not a valid name ({NAME_RULE}); rename the file or set `name`"
                ),
            );
        }
    }

    let prompt = body_of(body);
    if prompt.is_empty() {
        p.error("body", "empty: the body is the prompt");
    }
    for placeholder in expand::placeholders(prompt) {
        if !expand::BUILTINS.contains(&placeholder) && !declared.iter().any(|d| d == placeholder) {
            let issue = Issue::new(
                "body",
                format!(
                    "unknown placeholder {{{placeholder}}}; write {{{{ and }}}} for literal braces"
                ),
            );
            if !p.errors.contains(&issue) {
                p.errors.push(issue);
            }
        }
    }

    match (name, harness) {
        (Some(name), Some(harness)) if p.errors.is_empty() => Ok(Script {
            file: file.to_owned(),
            name,
            description,
            harness,
            model,
            tools,
            inputs,
            output,
            timeout,
            triggers,
            prompt: prompt.to_owned(),
            extra,
            warnings: p.warnings,
        }),
        (name, harness) => Err(Box::new(Invalid {
            file: file.to_owned(),
            named: name.is_some() || !map.contains_key("name"),
            name: name.unwrap_or(stem),
            harness,
            errors: p.errors,
            warnings: p.warnings,
        })),
    }
}

/// The body without the blank lines around it.
fn body_of(body: &str) -> &str {
    let mut rest = body;
    while let Some((line, tail)) = rest.split_once('\n') {
        if !line.trim().is_empty() {
            break;
        }
        rest = tail;
    }
    rest.trim_end()
}

const NAME_RULE: &str = "lowercase letters, digits and `-`, not starting with `-`, at most 64";

/// `[a-z0-9][a-z0-9-]*`, at most [`MAX_NAME_LEN`].
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `[a-z][a-z0-9_]*`.
fn is_input_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The text of an untagged string (`Value::as_str` also looks through YAML tags).
fn plain_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// A YAML value as one short text for a reason.
fn show(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{s:?}"),
        Value::Sequence(_) => "(an array)".into(),
        Value::Mapping(_) => "(a mapping)".into(),
        Value::Tagged(_) => "(a tagged value)".into(),
    }
}

/// Whether the relative `path` names something below the directory it is relative to, judged by its
/// text alone: not absolute (on any platform), and no `..` that climbs above it. Both `/` and `\`
/// separate. It follows no symlinks, and a caller that expands variables in a path checks the
/// result again.
pub fn path_stays_inside(path: &str) -> bool {
    let drive = matches!(path.as_bytes(), [d, b':', ..] if d.is_ascii_alphabetic());
    if drive || path.starts_with(['/', '\\']) {
        return false;
    }
    let mut depth = 0usize;
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            _ => depth += 1,
        }
    }
    depth > 0
}

/// Collects the problems while the keys are read.
#[derive(Default)]
struct Parser {
    errors: Vec<Issue>,
    warnings: Vec<Issue>,
}

impl Parser {
    fn error(&mut self, key: impl Into<String>, reason: impl Into<String>) {
        self.errors.push(Issue::new(key, reason));
    }

    fn unknown(&mut self, extra: &mut Mapping, at: &str, key: &str, value: &Value) {
        self.warnings.push(Issue::new(at, "unknown key"));
        extra.insert(Value::String(key.to_owned()), value.clone());
    }

    fn string(&mut self, key: &str, value: &Value) -> Option<String> {
        match value {
            Value::String(s) => Some(s.clone()),
            _ => {
                self.error(key, format!("must be a string, not {}", show(value)));
                None
            }
        }
    }

    fn non_empty(&mut self, key: &str, value: &Value) -> Option<String> {
        let s = self.string(key, value)?;
        if s.is_empty() {
            self.error(key, "must not be empty");
            return None;
        }
        Some(s)
    }

    fn name(&mut self, value: &Value) -> Option<String> {
        let s = self.string("name", value)?;
        if !is_name(&s) {
            self.error("name", format!("{s:?} is not a valid name ({NAME_RULE})"));
            return None;
        }
        Some(s)
    }

    fn tools(&mut self, value: &Value) -> Vec<String> {
        let Value::Sequence(items) = value else {
            self.error("tools", "must be an array of tool names");
            return Vec::new();
        };
        items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| self.non_empty(&format!("tools[{i}]"), item))
            .collect()
    }

    fn inputs(&mut self, value: &Value, declared: &mut Vec<String>) -> BTreeMap<String, Input> {
        let mut out = BTreeMap::new();
        let Value::Mapping(map) = value else {
            self.error("inputs", "must be a mapping of names to inputs");
            return out;
        };
        for (name, value) in map {
            let Some(name) = name.as_str() else {
                self.error("inputs", format!("name {} is not a string", show(name)));
                continue;
            };
            declared.push(name.to_owned());
            let key = format!("inputs.{name}");
            if expand::BUILTINS.contains(&name) {
                self.error(&key, "is the name of a built-in variable");
            } else if !is_input_name(name) {
                self.error(
                    &key,
                    "an input name is a lowercase letter, then lowercase letters, digits and `_`",
                );
            }
            if let Some(input) = self.input(&key, value) {
                out.insert(name.to_owned(), input);
            }
        }
        out
    }

    fn input(&mut self, at: &str, value: &Value) -> Option<Input> {
        let Value::Mapping(map) = value else {
            self.error(at, "must be a mapping (`{}` for a required input)");
            return None;
        };
        let (mut description, mut default, mut required) = (None, None, None);
        let mut extra = Mapping::new();
        for (key, value) in map {
            let Some(key) = key.as_str() else {
                self.error(at, format!("key {} is not a string", show(key)));
                continue;
            };
            let full = format!("{at}.{key}");
            match key {
                "description" => description = self.string(&full, value),
                "default" => default = self.string(&full, value),
                "required" => match value {
                    Value::Bool(b) => required = Some(*b),
                    _ => self.error(full, format!("must be true or false, not {}", show(value))),
                },
                _ => self.unknown(&mut extra, &full, key, value),
            }
        }
        Some(Input {
            required: required.unwrap_or(default.is_none()),
            description,
            default,
            extra,
        })
    }

    fn output(&mut self, value: &Value) -> Output {
        let mut out = Output::default();
        let Value::Mapping(map) = value else {
            self.error("output", "must be a mapping with `target` and `path`");
            return out;
        };
        let mut target_ok = true;
        for (key, value) in map {
            let Some(key) = key.as_str() else {
                self.error("output", format!("key {} is not a string", show(key)));
                continue;
            };
            match key {
                "target" => match plain_str(value) {
                    Some("show") => out.target = Target::Show,
                    Some("file") => out.target = Target::File,
                    Some("append") => out.target = Target::Append,
                    Some("replace") => out.target = Target::Replace,
                    _ => {
                        target_ok = false;
                        self.error(
                            "output.target",
                            format!("must be show, file, append or replace, not {}", show(value)),
                        );
                    }
                },
                "path" => out.path = self.non_empty("output.path", value),
                _ => self.unknown(&mut out.extra, &format!("output.{key}"), key, value),
            }
        }
        if target_ok && matches!(out.target, Target::File | Target::Append) {
            match &out.path {
                None if !map.contains_key("path") => {
                    self.error("output.path", "required for the targets file and append");
                }
                Some(path) if !path_stays_inside(path) => self.error(
                    "output.path",
                    format!("{path:?} does not name a file inside {{dir}}"),
                ),
                _ => {}
            }
        }
        out
    }

    fn timeout(&mut self, value: &Value) -> Option<Duration> {
        let parsed = plain_str(value).and_then(|s| {
            let unit = match s.chars().last()? {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => return None,
            };
            let digits = &s[..s.len() - 1];
            if digits.starts_with('0')
                || digits.is_empty()
                || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            // too many digits for a u64 is just as much over the limit
            let n: u64 = digits.parse().unwrap_or(u64::MAX);
            Some(Duration::from_secs(n.saturating_mul(unit)))
        });
        match parsed {
            Some(d) if d <= MAX_TIMEOUT => Some(d),
            Some(_) => {
                self.error("timeout", format!("{} is longer than 2h", show(value)));
                None
            }
            None => {
                self.error(
                    "timeout",
                    format!(
                        "must be a number with s, m or h (`90s`, `10m`), not {}",
                        show(value)
                    ),
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        parse(Path::new("s.md"), text)
            .expect_err("the script is invalid")
            .errors
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn crlf_a_bom_and_dashes_in_the_body() {
        let text = "\u{feff}---\r\nmodel: opus\r\n--- \r\n\r\n  indented\r\n---\r\nlast\r\n\r\n";
        let script = parse(Path::new("s.md"), text).unwrap();
        assert_eq!(script.model.as_deref(), Some("opus"));
        assert_eq!(script.prompt, "  indented\r\n---\r\nlast");
    }

    #[test]
    fn empty_frontmatter_is_not_a_mapping() {
        assert_eq!(
            errors("---\n---\nPrompt."),
            ["frontmatter: not a mapping of keys to values"]
        );
        assert_eq!(errors("")[0].split(':').next(), Some("frontmatter"));
    }

    #[test]
    fn every_problem_is_reported_and_placeholders_once() {
        let invalid = parse(
            Path::new("s.md"),
            "---\nharness: copilot\nmodel: 1\ntimeout: 3h\nextra: 1\n---\n{x} {x}",
        )
        .unwrap_err();
        let keys: Vec<&str> = invalid.errors.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["model", "timeout", "body"]);
        assert_eq!(invalid.harness.as_deref(), Some("copilot"));
        assert_eq!(invalid.name, "s");
        assert_eq!(invalid.warnings, [Issue::new("extra", "unknown key")]);
    }

    #[test]
    fn an_invalid_script_keeps_its_valid_name() {
        let invalid = parse(Path::new("s.md"), "---\nname: other\n---\n").unwrap_err();
        assert_eq!(invalid.name, "other");
    }

    #[test]
    fn a_tagged_value_is_invalid() {
        for front in [
            "format: !x 1",
            "name: !x a",
            "timeout: !x 5m",
            "output: { target: !x show }",
        ] {
            assert_eq!(errors(&format!("---\n{front}\n---\nPrompt.")).len(), 1);
        }
    }

    #[test]
    fn a_huge_timeout_is_too_long() {
        assert!(
            errors("---\ntimeout: 99999999999999999999h\n---\nPrompt.")[0]
                .ends_with("is longer than 2h")
        );
    }

    #[test]
    fn paths_inside_the_directory() {
        for path in ["a", "a/b.md", "./a", "a/../b", "a\\b", "{dir}x", "..a"] {
            assert!(path_stays_inside(path), "{path}");
        }
        for path in [
            "",
            ".",
            "a/..",
            "..",
            "../a",
            "a/../../b",
            "..\\a",
            "/a",
            "\\a",
            "\\\\host\\share",
            "C:\\a",
            "c:a",
            "{x}/../..",
        ] {
            assert!(!path_stays_inside(path), "{path}");
        }
    }
}
