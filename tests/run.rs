//! `agent-scripts run`, with the fake harness of `tests/helpers/fake_harness.rs` in place of an agent CLI.
//! Every directory of the runner (config, user scripts, state) is inside a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use agent_scripts::harness::Harness;
use agent_scripts::run::{Host, Request, Status};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-harness");

struct Setup {
    root: TempDir,
}

impl Setup {
    /// A work directory, and a user config whose harness `fake` is the fake harness.
    fn new() -> Setup {
        let setup = Setup {
            root: tempfile::tempdir().expect("temp dir"),
        };
        for dir in ["work", "config", "state", "bin"] {
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
        let mut unlimited = fake.clone();
        unlimited.as_object_mut().unwrap().remove("toolsArgs");
        let mut open = fake.clone();
        open.as_object_mut().unwrap().remove("isolateArgs");
        let mut missing = fake.clone();
        missing["command"] = json!(["no-such-program-agent-scripts"]);
        let config = json!({"harnesses": {
            "fake": fake, "unlimited": unlimited, "open": open, "missing": missing,
        }});
        setup.write("config/config.json", &config.to_string());
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

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.path(path)).unwrap()
    }

    /// A script of the harness `fake` with the frontmatter lines `front`.
    fn script(&self, front: &str, body: &str) -> PathBuf {
        let harness = if front.contains("harness:") {
            ""
        } else {
            "harness: fake\n"
        };
        self.write(
            "scripts/s.md",
            &format!("---\n{harness}{front}\n---\n{body}\n"),
        )
    }

    fn command(&self, script: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-scripts"));
        command
            .arg("run")
            .arg(script)
            .args(args)
            .current_dir(self.path("work"))
            .env("AGENT_SCRIPTS_CONFIG_DIR", self.path("config"))
            .env("AGENT_SCRIPTS_STATE_DIR", self.path("state"))
            .env("FAKE_RECORD", self.path("record"))
            .env_remove("AGENT_EVENTS_RUN")
            // the temporary directory is no repository, wherever it lies
            .env("GIT_CEILING_DIRECTORIES", self.root.path());
        command
    }

    fn run(&self, script: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(script.to_str().unwrap(), args)
            .envs(env.iter().copied())
            .output()
            .expect("the agent-scripts binary starts")
    }

    /// Runs with `--json` and returns the exit code and the record.
    fn json(&self, script: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.run(script, &all, env);
        let record = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", text(&out.stderr)));
        (out.status.code().expect("an exit code"), record)
    }

    fn started(&self) -> bool {
        self.path("record").exists()
    }

    /// The arguments the harness got.
    fn argv(&self) -> Vec<String> {
        let mut values: Vec<String> = self
            .read("record")
            .split_terminator('\0')
            .map(str::to_owned)
            .collect();
        values.truncate(values.len() - 2);
        values
    }

    /// Asserts a refusal before any harness started; returns the reason.
    fn refused(&self, script: &Path, args: &[&str]) -> String {
        let (code, record) = self.json(script, args, &[]);
        assert_eq!(
            (code, &record["status"]),
            (6, &json!("refused")),
            "{record}"
        );
        assert!(record.get("exitCode").is_none(), "{record}");
        assert!(!self.started(), "the harness started");
        text(&self.run(script, args, &[]).stderr)
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn wait_for(file: &Path) {
    let started = Instant::now();
    while !file.exists() {
        assert!(started.elapsed() < Duration::from_secs(20), "{file:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether nothing appends to `beat` any more.
fn stopped(beat: &Path) -> bool {
    let size = || fs::metadata(beat).expect("the child lived").len();
    let before = size();
    std::thread::sleep(Duration::from_millis(300));
    size() == before
}

#[test]
fn a_run_prints_the_text_and_passes_the_expanded_prompt() {
    let s = Setup::new();
    let script = s.script(
        "model: m1\ntools: [Read, \"Bash(git status)\", \"Bash(git log)\"]\ninputs: { who: {} }",
        "For {who}: {file} [{selection}] in {dir} {{x}}",
    );
    let out = s.run(
        &script,
        &[
            "--input",
            "who=a $(b) {file}",
            "--file",
            "n.md",
            "--selection",
            "-- sel",
        ],
        &[("FAKE_STDOUT", "answer\n"), ("FAKE_STDERR", "progress\n")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "answer\n");
    assert_eq!(text(&out.stderr), "progress\n");
    let dir = std::path::absolute(s.path("work")).unwrap();
    // the working directory of a process may come back with its symlinks resolved
    let prompt = s.argv().pop().unwrap();
    assert!(
        prompt.starts_with("For a $(b) {file}: n.md [-- sel] in "),
        "{prompt}"
    );
    assert!(prompt.ends_with("work {x}"), "{prompt}");
    let mut argv = s.argv();
    argv.pop();
    assert_eq!(
        argv,
        [
            "--isolate",
            "--model",
            "m1",
            "--tools",
            "Read,Bash",
            "--allow",
            "Read",
            "--allow",
            "Bash(git status)",
            "--allow",
            "Bash(git log)",
            "-p",
        ]
    );
    let record = s.read("record");
    let cwd = record.split_terminator('\0').rev().nth(1).unwrap();
    assert_eq!(
        Path::new(cwd).canonicalize().unwrap(),
        dir.canonicalize().unwrap()
    );
}

#[test]
fn dir_and_selection_file() {
    let s = Setup::new();
    fs::create_dir(s.path("other")).unwrap();
    let selection = s.write("selection", "from a file");
    let script = s.script("", "{selection}|{dir}");
    let (code, _) = s.json(
        &script,
        &[
            "--dir",
            s.path("other").to_str().unwrap(),
            "--selection-file",
            selection.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(code, 0);
    let prompt = s.argv().pop().unwrap();
    assert!(
        prompt.starts_with("from a file|") && prompt.ends_with("other"),
        "{prompt}"
    );
}

#[test]
fn the_built_in_claude_is_called_with_the_tools_of_the_script_only() {
    let s = Setup::new();
    let claude = s.path(if cfg!(windows) {
        "bin/claude.exe"
    } else {
        "bin/claude"
    });
    #[cfg(unix)]
    std::os::unix::fs::symlink(FAKE, &claude).unwrap();
    #[cfg(not(unix))]
    fs::copy(FAKE, &claude).map(|_| ()).unwrap();
    // no user config: the built-in definition
    fs::remove_file(s.path("config/config.json")).unwrap();
    for (front, tools) in [
        ("harness: claude", vec!["--tools", ""]),
        (
            "tools: [\"Bash(git status)\"]\nharness: claude",
            vec!["--tools", "Bash", "--allowedTools", "Bash(git status)"],
        ),
    ] {
        let script = s.script(front, "- a list");
        let (code, record) = s.json(&script, &[], &[("PATH", s.path("bin").to_str().unwrap())]);
        assert_eq!(code, 0, "{record}");
        let mut expected = vec!["--setting-sources", "user", "--strict-mcp-config"];
        expected.extend(tools);
        expected.extend(["-p", "\n- a list"]);
        assert_eq!(s.argv(), expected);
    }
}

#[test]
fn a_harness_that_cannot_be_limited_or_isolated_is_refused() {
    let s = Setup::new();
    for (harness, reason) in [
        ("copilot", ".github/copilot/settings.json"),
        ("open", "isolateArgs"),
        ("unlimited", "toolsArgs"),
        ("nope", "unknown harness"),
    ] {
        if harness == "copilot" {
            fs::rename(s.path("config/config.json"), s.path("kept")).unwrap();
        }
        let script = s.script(&format!("harness: {harness}"), "p");
        let got = s.refused(&script, &[]);
        assert!(got.contains(reason), "{harness}: {got}");
        if harness == "copilot" {
            fs::rename(s.path("kept"), s.path("config/config.json")).unwrap();
        }
    }
}

#[test]
fn a_script_cannot_ask_to_skip_the_permission_checks() {
    let s = Setup::new();
    for front in [
        "tools: [--dangerously-skip-permissions]",
        "tools: [Read, --allow-all]",
        "tools: [bypassPermissions]",
        "model: --allow-all-tools",
    ] {
        let script = s.script(front, "p");
        assert!(s.refused(&script, &[]).contains("not allowed"), "{front}");
    }
}

#[test]
fn what_the_runner_cannot_start_is_refused() {
    let s = Setup::new();
    // an invalid script, a missing file, an unknown name
    let script = s.script("timeout: 3h", "p");
    assert!(s.refused(&script, &[]).contains("longer than 2h"));
    s.refused(&s.path("scripts/none.md"), &[]);
    assert!(
        s.refused(Path::new("nothing"), &[])
            .contains("unknown script")
    );
    // a built-in variable without a value
    let script = s.script("", "{file}");
    assert!(s.refused(&script, &[]).contains("{file}"));
    // a prompt over the limit
    let big = s.write("big", &"a".repeat(256 * 1024));
    let script = s.script("", "x{selection}");
    s.refused(&script, &["--selection-file", big.to_str().unwrap()]);
    // the target replace belongs to a host
    let script = s.script("output: { target: replace }", "p");
    assert!(s.refused(&script, &[]).contains("replace"));
    // an output path that a value leads out of the directory
    let script = s.script(
        "inputs: { to: {} }\noutput: { target: file, path: \"{to}\" }",
        "p",
    );
    assert!(
        s.refused(&script, &["--input", "to=../x"])
            .contains("inside")
    );
    // a directory that does not exist
    let script = s.script("", "p");
    s.refused(&script, &["--dir", "no-such-dir"]);
    // a config file that cannot be understood
    s.write("config/config.json", "{");
    assert!(s.refused(&script, &[]).contains("not valid JSON"));
}

#[test]
fn usage_errors_exit_2() {
    let s = Setup::new();
    let script = s.script("inputs: { who: {} }", "{who}");
    let (code, record) = s.json(&script, &[], &[]);
    assert_eq!((code, &record["status"]), (2, &json!("refused")));
    for args in [
        &["--nope"][..],
        &["--input", "who"],
        &["--selection", "a", "--selection-file", "b"],
        &["--selection-file", "no-such-file"],
    ] {
        let out = s.run(&script, args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    assert!(!s.started());
}

#[test]
fn a_script_runs_by_its_name() {
    let s = Setup::new();
    s.write(
        "config/scripts/hello.md",
        "---\nharness: fake\n---\nfrom the user",
    );
    let name = Path::new("hello");
    assert_eq!(s.json(name, &[], &[]).0, 0);
    assert_eq!(s.argv().pop().unwrap(), "from the user");
    // the project script of that name wins
    fs::create_dir(s.path("work/.git")).unwrap();
    s.write(
        "work/.agents/scripts/hello.md",
        "---\nharness: fake\n---\nfrom the project",
    );
    assert_eq!(s.json(name, &[], &[]).0, 0);
    assert_eq!(s.argv().pop().unwrap(), "from the project");
}

#[test]
fn the_exit_codes_of_the_harness() {
    let s = Setup::new();
    let script = s.script("", "p");
    let (code, record) = s.json(&script, &[], &[("FAKE_STDOUT", "t")]);
    assert_eq!(code, 0);
    assert_eq!(record["status"], "ok");
    assert_eq!(record["exitCode"], 0);
    assert_eq!(record["text"], "t");
    assert!(record["durationMs"].is_u64());
    assert!(record.get("changedFiles").is_none(), "{record}");
    assert!(record.get("outputError").is_none());

    let (code, record) = s.json(&script, &[], &[("FAKE_EXIT", "7"), ("FAKE_STDOUT", "half")]);
    assert_eq!(code, 4);
    assert_eq!(record["status"], "error");
    assert_eq!(record["exitCode"], 7);
    assert_eq!(record["text"], "half");

    let script = s.script("harness: missing", "p");
    let (code, record) = s.json(&script, &[], &[]);
    assert_eq!((code, &record["status"]), (3, &json!("failed")));
    assert!(record.get("exitCode").is_none());
}

#[test]
fn the_expected_hash() {
    let s = Setup::new();
    let script = s.script("", "p");
    let hash: String = Sha256::digest(fs::read(&script).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let wrong = "0".repeat(64);
    assert!(
        s.refused(&script, &["--expect-sha256", &wrong])
            .contains(&hash)
    );
    for given in [hash.clone(), hash.to_uppercase()] {
        assert_eq!(s.json(&script, &["--expect-sha256", &given], &[]).0, 0);
    }
    fs::remove_file(s.path("record")).unwrap();
    s.script("", "changed");
    s.refused(&script, &["--expect-sha256", &hash]);
}

#[test]
fn the_target_file_never_overwrites() {
    let s = Setup::new();
    let script = s.script(
        "inputs: { n: {} }\noutput: { target: file, path: \"out/{n}.md\" }",
        "p",
    );
    let args = ["--input", "n=a"];
    let out = s.run(&script, &args, &[("FAKE_STDOUT", "first")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "first");
    assert_eq!(s.read("work/out/a.md"), "first");

    let (code, record) = s.json(&script, &args, &[("FAKE_STDOUT", "second")]);
    assert_eq!(code, 4);
    assert_eq!(record["status"], "error");
    assert_eq!(record["exitCode"], 0);
    assert_eq!(record["text"], "second");
    assert!(
        record["outputError"]
            .as_str()
            .unwrap()
            .contains("exists already")
    );
    assert_eq!(s.read("work/out/a.md"), "first");
}

#[test]
fn the_target_append_creates_and_appends() {
    let s = Setup::new();
    let script = s.script("output: { target: append, path: \"{dir}/log\" }", "p");
    for answer in ["one\n", "two\n"] {
        assert_eq!(s.json(&script, &[], &[("FAKE_STDOUT", answer)]).0, 0);
    }
    assert_eq!(s.read("work/log"), "one\ntwo\n");
    // a run that failed writes nothing
    let env = [("FAKE_STDOUT", "three"), ("FAKE_EXIT", "1")];
    assert_eq!(s.json(&script, &[], &env).0, 4);
    assert_eq!(s.read("work/log"), "one\ntwo\n");
}

#[cfg(unix)]
#[test]
fn an_output_never_follows_a_symlink_out_of_the_directory() {
    use std::os::unix::fs::symlink;
    let s = Setup::new();
    fs::create_dir(s.path("outside")).unwrap();
    let kept = s.write("outside/kept", "kept");
    symlink(s.path("outside"), s.path("work/dir")).unwrap();
    symlink(&kept, s.path("work/link")).unwrap();
    symlink(s.path("outside/new"), s.path("work/dangling")).unwrap();
    for target in ["file", "append"] {
        for path in [
            "dir/new",
            "dir/sub/new",
            "link",
            "dangling",
            // the directory before the `..` does not exist
            "none/../dir/new",
            "none/../link",
        ] {
            let script = s.script(
                &format!("output: {{ target: {target}, path: {path} }}"),
                "p",
            );
            let (code, record) = s.json(&script, &[], &[("FAKE_STDOUT", "t")]);
            assert_eq!(code, 4, "{target} {path}: {record}");
            assert!(record["outputError"].is_string());
        }
    }
    assert_eq!(fs::read_to_string(kept).unwrap(), "kept");
    assert_eq!(fs::read_dir(s.path("outside")).unwrap().count(), 1);
}

fn git(dir: &Path, args: &[&str]) {
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
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .status()
        .expect("git starts");
    assert!(status.success(), "git {args:?}");
}

#[test]
fn changed_files_are_those_the_run_changed() {
    let s = Setup::new();
    let work = s.path("work");
    for name in ["clean", "again", "dirty", "gone", "untouched"] {
        s.write(&format!("work/{name}"), "committed");
    }
    git(&work, &["init", "--quiet"]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "start"]);
    // dirty before the run
    s.write("work/again", "before");
    s.write("work/dirty", "before");
    s.write("work/old-untracked", "before");
    fs::create_dir(s.path("work/sub")).unwrap();
    let script = s.script("output: { target: file, path: answer }", "p");
    let (code, record) = s.json(
        &script,
        // paths are relative to the repository root also from below it
        &["--dir", "sub"],
        &[
            (
                "FAKE_WRITE",
                "../clean=new\n../again=after\n../dirty=before\nnew file=x",
            ),
            ("FAKE_DELETE", "../gone"),
        ],
    );
    assert_eq!(code, 0, "{record}");
    assert_eq!(
        record["changedFiles"],
        json!(["again", "clean", "gone", "sub/answer", "sub/new file"])
    );
}

#[cfg(unix)]
#[test]
fn a_filter_planted_during_the_run_is_never_started() {
    let s = Setup::new();
    let work = s.path("work");
    s.write("work/a", "committed");
    git(&work, &["init", "--quiet"]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "start"]);
    let marker = s.path("filtered");
    let config = format!(
        "{}[filter \"x\"]\n\tclean = touch '{}'; cat\n",
        s.read("work/.git/config"),
        marker.display()
    );
    let writes = format!(
        ".git/config={}\n.gitattributes=a filter=x\na=changed",
        // one line per file for the fake harness
        config.replace('\n', "\r")
    );
    let script = s.script("", "p");
    let (code, record) = s.json(&script, &[], &[("FAKE_WRITE", &writes)]);
    assert_eq!(code, 0, "{record}");
    assert!(!marker.exists(), "the filter ran");
    assert!(record.get("changedFiles").is_none(), "{record}");
}

#[cfg(unix)]
#[test]
fn a_remote_planted_during_the_run_is_never_asked() {
    let s = Setup::new();
    let work = s.path("work");
    s.write("work/a", "committed");
    git(&work, &["init", "--quiet"]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "start"]);
    let marker = s.path("fetched");
    let config = format!(
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tpartialClone = origin\n\
         [remote \"origin\"]\n\tpromisor = true\n\turl = \"ext::sh -c touch% {}\"\n\
         [protocol \"ext\"]\n\tallow = always\n",
        marker.display()
    );
    // a HEAD that names a missing object makes git fetch it
    let writes = format!(
        ".git/config={}\n.git/HEAD={}",
        config.replace('\n', "\r"),
        "1".repeat(40)
    );
    let script = s.script("", "p");
    let (code, record) = s.json(&script, &[], &[("FAKE_WRITE", &writes)]);
    assert_eq!(code, 0, "{record}");
    assert!(!marker.exists(), "the remote was asked");
    assert!(record.get("changedFiles").is_none(), "{record}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_orphan_in_a_session_of_its_own_ends_too() {
    let s = Setup::new();
    let script = s.script("", "p");
    let beat = s.path("beat");
    let (code, record) = s.json(
        &script,
        &[],
        &[
            ("FAKE_CHILD", beat.to_str().unwrap()),
            ("FAKE_CHILD_ORPHAN", "1"),
        ],
    );
    assert_eq!(code, 0, "{record}");
    assert!(stopped(&beat), "the orphan lives on");
}

#[test]
fn the_timeout_ends_the_process_tree() {
    let s = Setup::new();
    let script = s.script("timeout: 2s", "p");
    let beat = s.path("beat");
    let started = Instant::now();
    let (code, record) = s.json(
        &script,
        &[],
        &[
            ("FAKE_CHILD", beat.to_str().unwrap()),
            ("FAKE_STDOUT", "partial"),
            ("FAKE_SLEEP_MS", "60000"),
        ],
    );
    assert_eq!(
        (code, &record["status"]),
        (5, &json!("timeout")),
        "{record}"
    );
    assert_eq!(record["text"], "partial");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(stopped(&beat), "the child of the harness lives on");
}

#[cfg(unix)]
#[test]
fn a_child_that_ignores_sigterm_is_killed_after_the_grace_time() {
    let s = Setup::new();
    let script = s.script("timeout: 2s", "p");
    let beat = s.path("beat");
    let started = Instant::now();
    let (code, _) = s.json(
        &script,
        &[],
        &[
            ("FAKE_CHILD", beat.to_str().unwrap()),
            ("FAKE_CHILD_STUBBORN", "1"),
            // as the commands of an agent, in a session of its own
            ("FAKE_CHILD_SESSION", "1"),
            ("FAKE_SLEEP_MS", "60000"),
        ],
    );
    assert_eq!(code, 5);
    // 2 s timeout, then 5 s between SIGTERM and SIGKILL
    assert!(
        started.elapsed() >= Duration::from_secs(7),
        "{:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(stopped(&beat), "the child of the harness lives on");
}

#[test]
fn nothing_of_a_finished_run_stays_behind() {
    let s = Setup::new();
    let script = s.script("", "p");
    let beat = s.path("beat");
    let started = Instant::now();
    let (code, record) = s.json(
        &script,
        &[],
        &[
            ("FAKE_CHILD", beat.to_str().unwrap()),
            ("FAKE_STDOUT", "done"),
        ],
    );
    assert_eq!(code, 0, "{record}");
    assert_eq!(record["text"], "done");
    // the child held the pipes of the harness
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(stopped(&beat), "the child of the harness lives on");
}

/// A run that waits in the harness.
fn running(s: &Setup, script: &Path, env: &[(&str, &str)]) -> std::process::Child {
    let ready = s.path("ready");
    let child = s
        .command(script.to_str().unwrap(), &["--json"])
        .env("FAKE_STARTED", &ready)
        .env("FAKE_SLEEP_MS", "60000")
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the agent-scripts binary starts");
    wait_for(&ready);
    child
}

#[test]
fn one_run_per_script_and_a_crashed_run_leaves_no_lock() {
    let s = Setup::new();
    let script = s.script("timeout: 30s", "p");
    let mut first = running(&s, &script, &[("FAKE_SLEEP_MS", "20000")]);
    fs::remove_file(s.path("record")).unwrap();
    // in any directory, by any path to the same file
    fs::create_dir(s.path("other")).unwrap();
    let other = s.path("scripts/../scripts/s.md");
    let reason = s.refused(&other, &["--dir", s.path("other").to_str().unwrap()]);
    assert!(reason.contains("already running"), "{reason}");
    // another script is free
    let second = s.write("scripts/t.md", "---\nharness: fake\n---\np");
    assert_eq!(s.json(&second, &[], &[]).0, 0);
    // a crash
    first.kill().unwrap();
    first.wait().unwrap();
    // Linux ends the harness with its runner (Windows: the job object)
    #[cfg(target_os = "linux")]
    {
        let pid = s.read("ready");
        let started = Instant::now();
        while fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z "))
        {
            assert!(started.elapsed() < Duration::from_secs(5), "harness lives");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let (code, record) = s.json(&script, &[], &[]);
    assert_eq!(code, 0, "{record}");
}

#[cfg(unix)]
#[test]
fn a_signal_cancels_the_run_and_ends_the_tree() {
    for (signal, expected) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let s = Setup::new();
        let script = s.script("timeout: 30s", "p");
        let beat = s.path("beat");
        let run = running(&s, &script, &[("FAKE_CHILD", beat.to_str().unwrap())]);
        // the signal and process calls have no safe form in std
        #[allow(unsafe_code)]
        // SAFETY: kill sends a signal to our own child.
        unsafe {
            libc::kill(run.id() as libc::pid_t, signal)
        };
        let out = run.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(expected), "{}", text(&out.stderr));
        let record: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(record["status"], "cancelled");
        assert!(stopped(&beat), "the child of the harness lives on");
    }
}

#[test]
fn a_host_brings_definitions_applies_replace_and_cancels() {
    let s = Setup::new();
    let harness = Harness {
        id: "fake".into(),
        command: vec![FAKE.into()],
        print_args: vec!["{prompt}".into()],
        tools_args: vec!["--host={tools}".into()],
        isolate_args: vec!["--isolate".into()],
        ..Harness::default()
    };
    let host = Host {
        // wins over the user config
        harnesses: vec![harness],
        config_file: Some(s.path("config/config.json")),
        state_dir: Some(s.path("state")),
        replace: true,
        ..Host::default()
    };
    let request = Request {
        script: s
            .script("output: { target: replace }", "p")
            .display()
            .to_string(),
        dir: s.path("work"),
        ..Request::default()
    };
    // the fake harness of a host gets no environment of its own: it prints nothing and ends
    let outcome = agent_scripts::run::run(&request, &host, &Default::default());
    assert_eq!(outcome.record.status, Status::Ok, "{:?}", outcome.reason);

    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let outcome = agent_scripts::run::run(&request, &host, &cancelled);
    assert_eq!(outcome.record.status, Status::Cancelled);
    assert_eq!(outcome.record.exit_code, None);

    // without a state directory there is no lock, so nothing runs
    let host = Host {
        state_dir: None,
        ..host
    };
    let outcome = agent_scripts::run::run(&request, &host, &Default::default());
    assert_eq!(outcome.record.status, Status::Refused);
}
