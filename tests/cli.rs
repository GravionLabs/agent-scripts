//! Smoke tests of the `agent-scripts` binary. They start only the binary itself, never a harness or an agent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-scripts"))
        .args(args)
        .output()
        .expect("the agent-scripts binary starts")
}

#[test]
fn version_prints_the_crate_version() {
    let out = run(&["--version"]);
    assert!(out.status.success(), "exit status {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 output");
    assert_eq!(
        stdout.trim(),
        format!("agent-scripts {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_names_the_binary() {
    let out = run(&["--help"]);
    assert!(out.status.success(), "exit status {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 output");
    assert!(
        stdout.contains("Usage: agent-scripts"),
        "help output: {stdout}"
    );
}

#[test]
fn unknown_argument_is_a_usage_error() {
    let out = run(&["run"]);
    assert_eq!(out.status.code(), Some(2), "exit status {:?}", out.status);
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(path)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

#[test]
fn validate_accepts_a_valid_file() {
    let file = fixture("valid/summarize-file.md");
    let out = run(&["validate", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout)
            .trim()
            .ends_with("summarize-file.md: valid")
    );
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

#[test]
fn validate_warns_about_unknown_keys_and_succeeds() {
    let file = fixture("valid/unknown-keys.md");
    let out = run(&["validate", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("unknown-keys.md: warning: color: unknown key"),
        "{stderr}"
    );
}

#[test]
fn validate_exits_2_with_the_reason_for_every_invalid_fixture() {
    for entry in fs::read_dir(fixture("invalid")).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let expected = fs::read_to_string(file.with_extension("reason")).unwrap();
        let out = run(&["validate", file.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(2), "{}", file.display());
        let stderr = text(&out.stderr);
        for reason in expected.lines() {
            let line = format!("{}: {reason}", file.display());
            assert!(stderr.contains(&line), "{line:?} not in {stderr:?}");
        }
    }
}

#[test]
fn validate_of_a_missing_file_exits_2() {
    let out = run(&["validate", "does-not-exist.md"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("does-not-exist.md"));
}

/// A project with copies of some fixtures. The user config directory of the child points into the
/// same temporary directory where the platform allows it (not on Windows).
fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    let scripts = tmp.path().join(".agents/scripts");
    fs::create_dir_all(&scripts).unwrap();
    for file in [
        "valid/summarize-file.md",
        "valid/unknown-keys.md",
        "invalid/timeout-too-long.md",
    ] {
        let name = Path::new(file).file_name().unwrap();
        fs::copy(fixture(file), scripts.join(name)).unwrap();
    }
    tmp
}

fn list(home: &Path, args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-scripts"))
        .arg("list")
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .output()
        .expect("the agent-scripts binary starts")
}

/// The line of the table for `name`, with the column padding collapsed.
fn row(stdout: &str, name: &str) -> String {
    stdout
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|l| l.split(' ').next() == Some(name))
        .unwrap_or_else(|| panic!("no row {name} in {stdout}"))
}

#[test]
fn list_shows_name_source_harness_and_state() {
    let tmp = project();
    let sub = tmp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    // with the directory as argument, and from a subdirectory of the repository without one
    let by_argument = list(tmp.path(), &[tmp.path().to_str().unwrap()], tmp.path());
    let by_cwd = list(tmp.path(), &[], &sub);
    for out in [by_argument, by_cwd] {
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let stdout = text(&out.stdout);
        assert_eq!(
            row(&stdout, "summarize-file"),
            "summarize-file project claude valid"
        );
        assert_eq!(
            row(&stdout, "unknown-keys"),
            "unknown-keys project claude valid"
        );
        assert_eq!(
            row(&stdout, "timeout-too-long"),
            "timeout-too-long project claude invalid: timeout: \"121m\" is longer than 2h"
        );
        assert!(text(&out.stderr).contains("warning: color: unknown key"));
    }
}

// dirs::config_dir() follows these variables only on Unix.
#[cfg(unix)]
#[test]
fn list_merges_the_user_scripts_of_the_config_directory() {
    let tmp = project();
    let config = if cfg!(target_os = "macos") {
        tmp.path().join("Library/Application Support")
    } else {
        tmp.path().join("config")
    };
    let user = config.join("agent-scripts/scripts");
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("mine.md"), "---\nharness: copilot\n---\nHi.\n").unwrap();
    fs::write(user.join("summarize-file.md"), "---\n{}\n---\nHi.\n").unwrap();

    let out = list(tmp.path(), &[], tmp.path());
    let stdout = text(&out.stdout);
    assert_eq!(row(&stdout, "mine"), "mine user copilot valid");
    assert_eq!(
        row(&stdout, "summarize-file"),
        "summarize-file project claude valid"
    );
    assert_eq!(stdout.matches("summarize-file").count(), 1, "{stdout}");
}

#[test]
fn list_keeps_terminal_escapes_of_a_script_out_of_the_output() {
    let tmp = project();
    fs::write(
        tmp.path().join(".agents/scripts/escape.md"),
        "---\nharness: \"a\\e[31m\\nb\"\n\"k\\e]0;x\\a\": 1\n---\nHi.\n",
    )
    .unwrap();
    let out = list(tmp.path(), &[], tmp.path());
    for bytes in [&out.stdout, &out.stderr] {
        assert!(!bytes.contains(&0x1b) && !bytes.contains(&0x07));
    }
    assert_eq!(
        row(&text(&out.stdout), "escape"),
        "escape project a [31m b valid"
    );
}

#[test]
fn list_of_a_missing_directory_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let out = list(tmp.path(), &["missing"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("missing: not a directory"));
}
