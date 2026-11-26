//! The agent-events run of `agent-scripts run`, with the fake harness of
//! `tests/helpers/fake_harness.rs`. Every repository and directory is inside a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;

use agent_scripts::run::{Host, Request, Status};
use serde_json::{Value, json};
use tempfile::TempDir;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-harness");
const EVENTS: &str = "work/.git/.agents/events";

struct Setup {
    root: TempDir,
}

impl Setup {
    /// A work directory without a repository, and a user config with the harness `fake`.
    fn new() -> Setup {
        let setup = Setup {
            root: tempfile::tempdir().expect("temp dir"),
        };
        for dir in ["work", "config", "state"] {
            fs::create_dir(setup.path(dir)).unwrap();
        }
        let fake = json!({
            "command": [FAKE],
            "modelArgs": ["--model", "{model}"],
            "printArgs": ["-p", "{prompt}"],
            "toolsArgs": ["--tools", "{tools}"],
            "allowToolArgs": ["--allow", "{tool}"],
            "isolateArgs": ["--isolate"],
        });
        let mut missing = fake.clone();
        missing["command"] = json!(["no-such-program-agent-scripts"]);
        let config = json!({"harnesses": {"fake": fake, "missing": missing}});
        setup.write("config/config.json", &config.to_string());
        setup
    }

    /// The same with a repository in the work directory.
    fn repository() -> Setup {
        let setup = Setup::new();
        setup.git(&["init", "--quiet"]);
        setup
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }

    fn write(&self, path: &str, text: &str) -> PathBuf {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(self.path("work"))
            .env("GIT_CONFIG_GLOBAL", self.path("no-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdout(Stdio::null())
            .status()
            .expect("git starts");
        assert!(status.success(), "git {args:?}");
    }

    fn script(&self, front: &str) -> PathBuf {
        let harness = if front.contains("harness:") {
            ""
        } else {
            "harness: fake\n"
        };
        self.write(
            "scripts/hello.md",
            &format!("---\n{harness}{front}\n---\nthe secret prompt\n"),
        )
    }

    fn run(&self, script: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_agent-scripts"))
            .arg("run")
            .arg(script)
            .args(args)
            .current_dir(self.path("work"))
            .env("AGENT_SCRIPTS_CONFIG_DIR", self.path("config"))
            .env("AGENT_SCRIPTS_STATE_DIR", self.path("state"))
            // the temporary directory is no repository, wherever it lies
            .env("GIT_CEILING_DIRECTORIES", self.root.path())
            .env_remove("AGENT_EVENTS_RUN")
            .env_remove("AGENT_EVENTS_DIR")
            .envs(env.iter().copied())
            .output()
            .expect("the agent-scripts binary starts")
    }

    /// The run files below `dir`.
    fn files(&self, dir: &str) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.path(dir)) else {
            return Vec::new();
        };
        entries.map(|e| e.unwrap().path()).collect()
    }

    /// The events of the only run file, checked against the schema and the rules across lines.
    fn events(&self) -> Vec<Value> {
        let files = self.files(EVENTS);
        assert_eq!(files.len(), 1, "{files:?}");
        checked(&files[0])
    }
}

fn schema() -> jsonschema::Validator {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/agent-events/core.schema.json");
    let schema: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .expect("the schema is a JSON schema")
}

fn checked(file: &Path) -> Vec<Value> {
    let schema = schema();
    let text = fs::read_to_string(file).unwrap();
    assert!(text.ends_with('\n'), "{text:?}");
    let events: Vec<Value> = text
        .lines()
        .map(|line| {
            assert!(line.len() <= 16384);
            let event: Value = serde_json::from_str(line).expect("a line is JSON");
            let errors: Vec<String> = schema.iter_errors(&event).map(|e| e.to_string()).collect();
            assert!(errors.is_empty(), "{line}: {errors:?}");
            event
        })
        .collect();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], json!(index + 1), "{text}");
        assert_eq!(
            event["run"].as_str(),
            file.file_stem().unwrap().to_str(),
            "{text}"
        );
    }
    events
}

fn types(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["type"].as_str().unwrap()).collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Asserts that both ends of `events` have `outcome`, `reason` and `tone`.
fn ended(events: &[Value], outcome: &str, reason: &str, tone: &str) {
    assert_eq!(
        types(events),
        [
            "run.started",
            "task.started",
            "task.finished",
            "run.finished"
        ]
    );
    for end in &events[2..] {
        assert_eq!(
            (&end["outcome"], &end["reason"], &end["tone"]),
            (&json!(outcome), &json!(reason), &json!(tone)),
            "{end}"
        );
    }
}

#[test]
fn a_run_in_a_repository_writes_one_run() {
    let s = Setup::repository();
    let script = s.script("model: m1");
    let out = s.run(&script, &[], &[("FAKE_STDOUT", "the secret answer")]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let events = s.events();
    ended(&events, "succeeded", "done", "success");
    let subject = json!({"kind": "agent-scripts.script", "id": "hello", "title": "hello"});
    assert_eq!(events[0]["subject"], subject);
    assert_eq!(events[0]["profiles"], json!([]));
    assert_eq!(
        events[0]["agent"],
        json!({"harness": "fake", "model": "m1"})
    );
    let pid = events[0]["host"]["jsonl-file.pid"].as_str().unwrap();
    assert!(pid.parse::<u32>().is_ok(), "{pid}");
    assert_eq!(events[1]["subject"], subject);
    assert_eq!(events[1]["task"], events[2]["task"]);
    let text = fs::read_to_string(&s.files(EVENTS)[0]).unwrap();
    assert!(!text.contains("secret"), "{text}");

    // a second run has a file of its own and leaves the first as it is
    let first = s.files(EVENTS)[0].clone();
    assert_eq!(s.run(&script, &[], &[]).status.code(), Some(0));
    assert_eq!(s.files(EVENTS).len(), 2);
    assert_eq!(fs::read_to_string(first).unwrap(), text);
}

#[test]
fn the_end_has_the_status_of_the_run() {
    for (front, env, code, outcome, reason) in [
        ("", ("FAKE_EXIT", "1"), 4, "failed", "error"),
        (
            "harness: missing",
            ("FAKE_EXIT", "0"),
            3,
            "failed",
            "failed",
        ),
        (
            "timeout: 1s",
            ("FAKE_SLEEP_MS", "30000"),
            5,
            "failed",
            "timeout",
        ),
        (
            "output: { target: replace }",
            ("FAKE_EXIT", "0"),
            6,
            "failed",
            "refused",
        ),
    ] {
        let s = Setup::repository();
        let out = s.run(&s.script(front), &[], &[env]);
        assert_eq!(out.status.code(), Some(code), "{}", stderr(&out));
        ended(&s.events(), outcome, reason, "danger");
    }
}

#[test]
fn a_cancelled_run_of_a_host() {
    let s = Setup::repository();
    let host = Host {
        config_file: Some(s.path("config/config.json")),
        state_dir: Some(s.path("state")),
        events: true,
        events_run: Some("host.run_1".into()),
        ..Host::default()
    };
    let request = Request {
        script: s.script("").to_str().unwrap().into(),
        dir: s.path("work"),
        ..Request::default()
    };
    let outcome = agent_scripts::run::run(&request, &host, &AtomicBool::new(true));
    assert_eq!(outcome.record.status, Status::Cancelled);
    assert_eq!(outcome.events_warning, None);
    let events = s.events();
    assert_eq!(events[0]["run"], "host.run_1");
    ended(&events, "cancelled", "cancelled", "warning");
}

#[test]
fn the_run_id_of_the_environment() {
    let s = Setup::repository();
    let script = s.script("");
    let env = [("AGENT_EVENTS_RUN", "my-run.1")];
    assert_eq!(s.run(&script, &[], &env).status.code(), Some(0));
    assert_eq!(s.events()[0]["run"], "my-run.1");
    let file = s.path(EVENTS).join("my-run.1.jsonl");
    let first = fs::read_to_string(&file).unwrap();

    // the file is another run's: a run starts only once
    let out = s.run(&script, &[], &env);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("warning: no events"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), first);

    // an id that would leave the directory
    let out = s.run(&script, &[], &[("AGENT_EVENTS_RUN", "../out")]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("not a run id"), "{}", stderr(&out));
    assert_eq!(s.files(EVENTS).len(), 1);
    assert!(!s.path("work/.git/.agents/out.jsonl").exists());

    // an empty one is not set
    assert_eq!(
        s.run(&script, &[], &[("AGENT_EVENTS_RUN", "")])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(s.files(EVENTS).len(), 2);
}

#[test]
fn the_directory_of_the_environment() {
    let s = Setup::repository();
    let script = s.script("");
    let dir = s.path("elsewhere");
    let env = [("AGENT_EVENTS_DIR", dir.to_str().unwrap())];
    let out = s.run(&script, &[], &env);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let files = s.files("elsewhere");
    assert_eq!(files.len(), 1, "{files:?}");
    ended(&checked(&files[0]), "succeeded", "done", "success");
    assert!(!s.path("work/.git/.agents").exists());

    // a relative one would lie in the repository
    let out = s.run(&script, &[], &[("AGENT_EVENTS_DIR", "relative")]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stderr(&out).contains("not an absolute path"),
        "{}",
        stderr(&out)
    );
    assert!(!s.path("work/relative").exists());
    assert!(!s.path("work/.git/.agents").exists());

    // an empty one is not set
    assert_eq!(
        s.run(&script, &[], &[("AGENT_EVENTS_DIR", "")])
            .status
            .code(),
        Some(0)
    );
    ended(&s.events(), "succeeded", "done", "success");

    // outside a working tree it changes nothing
    let s = Setup::new();
    let dir = s.path("elsewhere");
    let env = [("AGENT_EVENTS_DIR", dir.to_str().unwrap())];
    assert_eq!(s.run(&s.script(""), &[], &env).status.code(), Some(0));
    assert!(!dir.exists());
}

#[test]
fn a_worktree_writes_to_the_common_directory() {
    let s = Setup::repository();
    s.git(&["commit", "--quiet", "--allow-empty", "-m", "start"]);
    s.git(&["worktree", "add", "--quiet", "--detach", "../linked"]);
    fs::create_dir(s.path("linked/sub")).unwrap();
    let script = s.script("");
    for dir in ["linked/sub", "work"] {
        let dir = s.path(dir);
        let out = s.run(&script, &["--dir", dir.to_str().unwrap()], &[]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }
    let files = s.files(EVENTS);
    assert_eq!(files.len(), 2, "{files:?}");
    files
        .iter()
        .for_each(|f| ended(&checked(f), "succeeded", "done", "success"));
}

#[test]
fn nothing_is_written_with_no_events_or_outside_a_working_tree() {
    let s = Setup::repository();
    let script = s.script("");
    let out = s.run(&script, &["--no-events"], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!s.path("work/.git/.agents").exists());
    // the git directory itself is no working tree
    let git_dir = s.path("work/.git");
    let out = s.run(&script, &["--dir", git_dir.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!s.path("work/.git/.agents").exists());

    let s = Setup::new();
    let out = s.run(&s.script(""), &[], &[("AGENT_EVENTS_RUN", "r")]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(fs::read_dir(s.path("work")).unwrap().count(), 0);
}

#[test]
fn a_failed_write_is_a_warning_and_the_run_goes_on() {
    let s = Setup::repository();
    // a file where the directory belongs
    s.write("work/.git/.agents", "");
    let out = s.run(&s.script(""), &[], &[("FAKE_STDOUT", "answer")]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(out.stdout, b"answer");
    assert!(
        stderr(&out).starts_with("warning: no events"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_schema_check_rejects_a_wrong_line() {
    let good = json!({
        "v": 1, "seq": 1, "ts": "2026-11-26T00:00:00.000Z", "run": "r",
        "type": "run.finished", "tone": "success", "outcome": "succeeded",
    });
    let schema = schema();
    assert!(schema.is_valid(&good));
    for (key, value) in [("ts", "yesterday"), ("outcome", "done"), ("run", "a/b")] {
        let mut bad = good.clone();
        bad[key] = json!(value);
        assert!(!schema.is_valid(&bad), "{key}");
    }
}
