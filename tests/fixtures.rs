//! Every fixture against the Rust parser and against `schema/agent-script.schema.json`, so the two
//! cannot drift. The tests only read the repository.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_scripts::dir::{Loaded, load};
use agent_scripts::format::{self, Target};
use serde_json::Value;

/// Invalid fixtures the schema accepts or cannot judge: the rules only the parser has (the body,
/// the file name, the directory, YAML itself, the path inside `{dir}` and the 2h limit).
const PARSER_ONLY: &[&str] = &[
    "Bad_File_Name",
    "broken-yaml",
    "duplicate-a",
    "duplicate-b",
    "duplicate-key",
    "empty-body",
    "no-frontmatter",
    "path-absolute",
    "path-escapes",
    "timeout-too-long",
    "unclosed-frontmatter",
    "unknown-placeholder",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures(kind: &str) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(root().join("fixtures").join(kind))
        .expect("the fixture directory")
        .map(|e| e.expect("a directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no {kind} fixtures");
    out
}

fn schema() -> jsonschema::Validator {
    let text = fs::read_to_string(root().join("schema/agent-script.schema.json")).unwrap();
    let schema: Value = serde_json::from_str(&text).expect("the schema is JSON");
    jsonschema::draft202012::new(&schema).expect("the schema is a JSON schema")
}

/// The frontmatter as JSON; `None` when there is none or it has no JSON form.
fn frontmatter(text: &str) -> Option<Value> {
    let (front, _) = format::split_frontmatter(text).ok()?;
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(front).ok()?;
    to_json(&yaml)
}

fn to_json(value: &serde_yaml_ng::Value) -> Option<Value> {
    use serde_yaml_ng::Value as Yaml;
    Some(match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(b) => (*b).into(),
        Yaml::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => serde_json::Number::from_f64(n.as_f64()?)?.into(),
        },
        Yaml::String(s) => s.clone().into(),
        Yaml::Sequence(items) => Value::Array(items.iter().map(to_json).collect::<Option<_>>()?),
        Yaml::Mapping(map) => Value::Object(
            map.iter()
                .map(|(k, v)| match k {
                    Yaml::String(k) => Some((k.clone(), to_json(v)?)),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        ),
        Yaml::Tagged(_) => return None,
    })
}

fn loaded(path: &Path) -> Loaded {
    load(path).expect("the fixture can be read")
}

#[test]
fn valid_fixtures_pass_the_parser_and_the_schema() {
    let schema = schema();
    for (name, path) in fixtures("valid") {
        if let Err(invalid) = loaded(&path) {
            panic!("{name}: {:?}", invalid.errors);
        }
        let front = frontmatter(&fs::read_to_string(&path).unwrap()).expect("frontmatter");
        let errors: Vec<String> = schema.iter_errors(&front).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{name}: the schema rejects: {errors:?}");
    }
}

#[test]
fn invalid_fixtures_fail_with_the_expected_reason() {
    for (name, path) in fixtures("invalid") {
        let expected = fs::read_to_string(path.with_extension("reason"))
            .unwrap_or_else(|e| panic!("{name}.reason: {e}"));
        let Err(invalid) = loaded(&path) else {
            panic!("{name} is valid");
        };
        let errors: Vec<String> = invalid.errors.iter().map(ToString::to_string).collect();
        let expected: Vec<&str> = expected.lines().collect();
        assert_eq!(errors.len(), expected.len(), "{name}: {errors:?}");
        for (error, expected) in errors.iter().zip(expected) {
            assert!(
                error.starts_with(expected),
                "{name}: {error:?} does not start with {expected:?}"
            );
        }
        assert_eq!(invalid.file, path);
    }
}

#[test]
fn the_schema_rejects_exactly_what_it_can_express() {
    let schema = schema();
    for (name, path) in fixtures("invalid") {
        let front = frontmatter(&fs::read_to_string(&path).unwrap());
        let rejected = match &front {
            Some(front) => !schema.is_valid(front),
            // a key that is no string has no JSON form, so no schema accepts it
            None => name == "key-not-string",
        };
        assert_eq!(
            rejected,
            !PARSER_ONLY.contains(&name.as_str()),
            "{name}: the schema and the parser drifted apart"
        );
    }
}

/// Boundaries of single keys, each judged by the parser and the schema.
#[test]
fn parser_and_schema_agree_on_boundary_values() {
    let name64 = format!("name: {}", "a".repeat(64));
    let cases: &[(&str, bool)] = &[
        ("{}", true),
        ("format: 1.0", true),
        ("format: \"1\"", false),
        ("format: 0", false),
        (&name64, true),
        ("name: 0-a", true),
        ("name: -a", false),
        ("name: \"\"", false),
        ("name: 1", false),
        ("description: \"\"", true),
        ("harness: other", true),
        ("model: \"\"", false),
        ("tools: []", true),
        ("tools: [1]", false),
        ("tools: null", false),
        ("inputs: {}", true),
        ("inputs: { a_1: {} }", true),
        ("inputs: { _a: {} }", false),
        ("inputs: { 1a: {} }", false),
        ("inputs: { a-b: {} }", false),
        ("inputs: { file: {} }", false),
        ("inputs: { dir: {} }", false),
        ("inputs: { a: null }", false),
        ("inputs: { a: { description: [] } }", false),
        ("output: {}", true),
        ("output: { target: show }", true),
        ("output: { target: replace }", true),
        ("output: { target: show, path: x }", true),
        ("output: { target: show, path: \"\" }", false),
        ("output: { target: file, path: a/b }", true),
        ("output: { target: null }", false),
        ("timeout: 1s", true),
        ("timeout: 2h", true),
        ("timeout: 0s", false),
        ("timeout: 01m", false),
        ("timeout: 1.5h", false),
        ("timeout: 1d", false),
        ("timeout: m", false),
        ("timeout: \"\"", false),
        ("timeout: 5 m", false),
        ("triggers: [{ cron: x }]", true),
        ("triggers: {}", false),
        ("anything: else", true),
    ];
    let schema = schema();
    for (front, valid) in cases {
        let text = format!("---\n{front}\n---\nPrompt.\n");
        let parsed = format::parse(Path::new("case.md"), &text);
        assert_eq!(parsed.is_ok(), *valid, "parser: {front}: {parsed:?}");
        let json = frontmatter(&text).expect("frontmatter");
        assert_eq!(schema.is_valid(&json), *valid, "schema: {front}");
    }
}

#[test]
fn the_example_of_the_adr_is_parsed_into_its_values() {
    let script = loaded(&root().join("fixtures/valid/summarize-file.md")).unwrap();
    assert_eq!(script.name, "summarize-file");
    assert_eq!(
        script.description.as_deref(),
        Some("Summary of a file in five bullet points")
    );
    assert_eq!(script.harness, "claude");
    assert_eq!(script.model.as_deref(), Some("sonnet"));
    assert_eq!(script.tools, ["Read"]);
    let audience = &script.inputs["audience"];
    assert_eq!(audience.default.as_deref(), Some("developers"));
    assert!(!audience.required);
    assert_eq!(script.output.target, Target::Show);
    assert_eq!(script.timeout, Duration::from_secs(300));
    assert_eq!(
        script.prompt,
        "Summarize {file} for {audience} in five bullet points."
    );
    assert!(script.warnings.is_empty());
}

#[test]
fn every_key_has_its_default() {
    let script = loaded(&root().join("fixtures/valid/minimal.md")).unwrap();
    assert_eq!(script.name, "minimal");
    assert_eq!(script.description, None);
    assert_eq!(script.harness, "claude");
    assert_eq!(script.model, None);
    assert!(script.tools.is_empty());
    assert!(script.inputs.is_empty());
    assert_eq!(script.output.target, Target::Show);
    assert_eq!(script.output.path, None);
    assert_eq!(script.timeout, Duration::from_secs(600));
    assert!(script.triggers.is_empty());
}

#[test]
fn every_key_is_read() {
    let script = loaded(&root().join("fixtures/valid/all-keys.md")).unwrap();
    assert_eq!(script.name, "release-notes");
    assert_eq!(script.harness, "copilot");
    assert_eq!(script.tools, ["shell(git:*)", "Read"]);
    assert!(script.inputs["version"].required);
    assert!(!script.inputs["tone"].required);
    assert!(script.inputs["scope"].required);
    assert_eq!(script.output.target, Target::Append);
    assert_eq!(script.output.path.as_deref(), Some("notes/{version}.md"));
    assert_eq!(script.timeout, Duration::from_secs(7200));
}

#[test]
fn unknown_keys_are_kept_with_a_warning_each() {
    let path = root().join("fixtures/valid/unknown-keys.md");
    let script = loaded(&path).unwrap();
    let warnings: Vec<String> = script.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        warnings,
        [
            "color: unknown key",
            "inputs.who.kind: unknown key",
            "output.mode: unknown key"
        ]
    );
    assert_eq!(script.file, path);
    assert_eq!(script.extra["color"], "blue");
    assert_eq!(script.inputs["who"].extra["kind"], "text");
    assert_eq!(script.output.extra["mode"], "0600");
}
