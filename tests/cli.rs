//! Smoke tests of the `agent-scripts` binary. They start only the binary itself, never a harness or an agent.

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
