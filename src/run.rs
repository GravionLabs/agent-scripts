//! Running a script (ADR 0019 "Runner"): one read of the file, the checks, the harness in print
//! mode, the output target and the result record.

use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::expand::{self, ExpandError, Values};
use crate::format::{Issue, Script, Target};
use crate::harness::{self, Call, Harness};
use crate::{changes, dir, output, process};

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The harness exited non-zero, or the output target failed.
    Error,
    /// The harness could not be started.
    Failed,
    Timeout,
    Cancelled,
    /// Not started by the runner.
    Refused,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Error => "error",
            Status::Failed => "failed",
            Status::Timeout => "timeout",
            Status::Cancelled => "cancelled",
            Status::Refused => "refused",
        }
    }
}

/// The result record of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub status: Status,
    /// The exit code of the harness; `None` when it did not start or a signal ended it.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// The answer of the agent.
    pub text: String,
    /// Relative to the repository root; `None` outside a git working tree or without a start.
    pub changed_files: Option<Vec<String>>,
    pub output_error: Option<String>,
}

impl Record {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("status".into(), json!(self.status.as_str()));
        if let Some(code) = self.exit_code {
            map.insert("exitCode".into(), json!(code));
        }
        map.insert("durationMs".into(), json!(self.duration_ms));
        map.insert("text".into(), json!(self.text));
        if let Some(files) = &self.changed_files {
            map.insert("changedFiles".into(), json!(files));
        }
        if let Some(error) = &self.output_error {
            map.insert("outputError".into(), json!(error));
        }
        Value::Object(map)
    }
}

/// A run: the record and what a host shows beside it.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub record: Record,
    /// Why the status is not `ok`, when the runner knows.
    pub reason: Option<String>,
    /// The run was refused because the caller gave too little (a required input): a usage error.
    pub usage: bool,
    /// The warnings of the script file.
    pub warnings: Vec<Issue>,
    /// What the harness wrote to its standard error.
    pub stderr: String,
}

impl Outcome {
    fn new(status: Status, reason: Option<String>) -> Outcome {
        Outcome {
            record: Record {
                status,
                exit_code: None,
                duration_ms: 0,
                text: String::new(),
                changed_files: None,
                output_error: None,
            },
            reason,
            usage: false,
            warnings: Vec::new(),
            stderr: String::new(),
        }
    }
}

fn refused(reason: impl Into<String>) -> Outcome {
    Outcome::new(Status::Refused, Some(reason.into()))
}

/// What one run is asked to do.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// The path of a script file, or the name of a project or user script. A name has only
    /// lowercase letters, digits and `-`; write `./name` for a file without `.md` in its name.
    pub script: String,
    /// The working directory of the run.
    pub dir: PathBuf,
    /// `{file}`, as the prompt gets it.
    pub file: Option<String>,
    /// `{selection}`.
    pub selection: Option<String>,
    pub inputs: BTreeMap<String, String>,
    /// Refuse a file whose content has another SHA-256 (hex).
    pub expect_sha256: Option<String>,
}

/// What the program that runs scripts brings.
#[derive(Debug, Clone, Default)]
pub struct Host {
    /// Definitions that replace those of the user config and the built-ins by id.
    pub harnesses: Vec<Harness>,
    /// The user config file with `harnesses`.
    pub config_file: Option<PathBuf>,
    /// The directory of the user scripts.
    pub user_scripts: Option<PathBuf>,
    /// The directory of the run locks. Without one nothing runs.
    pub state_dir: Option<PathBuf>,
    /// The host applies the target `replace` itself, with the text of the record. Without it such a
    /// script is refused.
    pub replace: bool,
}

impl Host {
    /// The files and directories of the user.
    pub fn user() -> Host {
        Host {
            harnesses: Vec::new(),
            config_file: dir::user_config_dir().map(|d| d.join("config.json")),
            user_scripts: dir::user_scripts_dir(),
            state_dir: dir::state_dir(),
            replace: false,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The script file of a path or a name.
fn locate(request: &Request, host: &Host) -> Result<(PathBuf, Option<String>), String> {
    let script = &request.script;
    let is_name = !script.is_empty()
        && script
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !is_name {
        return Ok((PathBuf::from(script), None));
    }
    let project = dir::project_root(&request.dir).join(dir::PROJECT_SCRIPTS);
    let entries =
        dir::list(Some(&project), host.user_scripts.as_deref()).map_err(|e| e.to_string())?;
    // several files of one name are all invalid; the first one tells why
    let entry = entries
        .into_iter()
        .find(|e| e.name() == script)
        .ok_or_else(|| format!("unknown script {script:?}"))?;
    let file = match entry.script {
        Ok(s) => s.file,
        Err(i) => i.file,
    };
    Ok((file, Some(script.clone())))
}

fn reasons(file: &Path, errors: &[Issue]) -> String {
    let errors: Vec<String> = errors.iter().map(Issue::to_string).collect();
    format!("{}: invalid: {}", file.display(), errors.join("; "))
}

/// Reads the script once and checks it: the content whose hash is compared is the content that runs.
/// Returns the script and the canonical path of its file.
fn load(request: &Request, host: &Host) -> Result<(Script, PathBuf), Box<Outcome>> {
    let (file, name) = locate(request, host).map_err(|e| Box::new(refused(e)))?;
    let unreadable = |e: std::io::Error| Box::new(refused(format!("{}: {e}", file.display())));
    let canonical = file.canonicalize().map_err(unreadable)?;
    if !canonical.is_file() {
        return Err(Box::new(refused(format!("{}: not a file", file.display()))));
    }
    let bytes = fs::read(&canonical).map_err(unreadable)?;
    if let Some(expected) = &request.expect_sha256 {
        let actual = hex(&Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(Box::new(refused(format!(
                "{}: its SHA-256 is {actual}, not the expected {expected}",
                file.display()
            ))));
        }
    }
    let script = match dir::load_bytes(&file, bytes).map_err(unreadable)? {
        Ok(script) => script,
        Err(invalid) => {
            let mut outcome = refused(reasons(&invalid.file, &invalid.errors));
            outcome.warnings = invalid.warnings;
            return Err(Box::new(outcome));
        }
    };
    if name.is_some_and(|n| n != script.name) {
        return Err(Box::new(refused(format!(
            "{}: changed while it was looked up",
            file.display()
        ))));
    }
    Ok((script, canonical))
}

/// Takes the lock of the script at `canonical`. The operating system releases it with the file, so a
/// run that crashed leaves none.
fn lock(state_dir: Option<&Path>, canonical: &Path) -> Result<File, String> {
    let locks = state_dir
        .ok_or("there is no state directory for the run lock")?
        .join("locks");
    fs::create_dir_all(&locks).map_err(|e| format!("{}: {e}", locks.display()))?;
    let key = hex(&Sha256::digest(canonical.as_os_str().as_encoded_bytes()));
    let path = locks.join(format!("{key}.lock"));
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err("a run of this script is already running".into()),
        Err(TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

/// Runs the script of `request` and returns when it ended. Setting `cancel` ends the run.
pub fn run(request: &Request, host: &Host, cancel: &AtomicBool) -> Outcome {
    let started = Instant::now();
    let (script, canonical) = match load(request, host) {
        Ok(loaded) => loaded,
        Err(outcome) => return *outcome,
    };
    let mut outcome = execute(&script, &canonical, request, host, cancel);
    outcome.warnings = script.warnings;
    outcome.record.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    outcome
}

fn execute(
    script: &Script,
    canonical: &Path,
    request: &Request,
    host: &Host,
    cancel: &AtomicBool,
) -> Outcome {
    let harness = match harness::find(
        &script.harness,
        &host.harnesses,
        host.config_file.as_deref(),
    ) {
        Ok(harness) => harness,
        Err(e) => return refused(e),
    };
    let dir = match std::path::absolute(&request.dir) {
        Ok(dir) if dir.is_dir() => dir,
        _ => {
            return refused(format!("{}: not a directory", request.dir.display()));
        }
    };
    let values = Values {
        file: request.file.clone(),
        selection: request.selection.clone(),
        dir: dir.to_str().map(str::to_owned),
        inputs: request.inputs.clone(),
    };
    let expanded = |template: &str| {
        expand::expand(template, &script.inputs, &values).map_err(|e| {
            let mut outcome = refused(e.to_string());
            outcome.usage = matches!(e, ExpandError::MissingInput(_));
            Box::new(outcome)
        })
    };
    let prompt = match expanded(&script.prompt) {
        Ok(prompt) => prompt,
        Err(outcome) => return *outcome,
    };
    // the output path is settled before the harness starts
    let target = match script.output.target {
        Target::Show => None,
        Target::Replace if host.replace => None,
        Target::Replace => {
            return refused("the output target replace is not available here");
        }
        target @ (Target::File | Target::Append) => {
            let path = match expanded(script.output.path.as_deref().unwrap_or_default()) {
                Ok(path) => path,
                Err(outcome) => return *outcome,
            };
            match output::resolve(&dir, &path) {
                Ok(file) => Some((file, target == Target::Append)),
                Err(e) => return refused(e),
            }
        }
    };
    let argv = match harness.argv(Call {
        model: script.model.as_deref(),
        tools: &script.tools,
        prompt: &prompt,
    }) {
        Ok(argv) => argv,
        Err(e) => return refused(e),
    };
    // held until the end of the run
    let _lock = match lock(host.state_dir.as_deref(), canonical) {
        Ok(lock) => lock,
        Err(e) => return refused(e),
    };
    let before = changes::snapshot(&dir, None);
    if cancel.load(Ordering::SeqCst) {
        return Outcome::new(Status::Cancelled, None);
    }
    let finished = match process::run(&argv, &dir, script.timeout, cancel) {
        Ok(finished) => finished,
        Err(e) => return Outcome::new(Status::Failed, Some(e)),
    };
    let (status, exit_code, reason) = match finished.end {
        // a signal to the runner may end the harness before the runner sees it
        _ if cancel.load(Ordering::SeqCst) => (Status::Cancelled, None, None),
        process::End::Cancelled => (Status::Cancelled, None, None),
        process::End::TimedOut => (
            Status::Timeout,
            None,
            Some(format!(
                "the harness ran longer than {} s",
                script.timeout.as_secs()
            )),
        ),
        process::End::Exited(exit) if exit.success() => (Status::Ok, exit.code(), None),
        process::End::Exited(exit) => (
            Status::Error,
            exit.code(),
            Some(format!("the harness failed: {exit}")),
        ),
    };
    let mut outcome = Outcome::new(status, reason);
    outcome.stderr = String::from_utf8_lossy(&finished.stderr).into_owned();
    outcome.record.exit_code = exit_code;
    outcome.record.text = String::from_utf8_lossy(&finished.stdout).into_owned();
    if status == Status::Ok
        && let Some((file, append)) = target
        && let Err(e) = output::write(&dir, &file, outcome.record.text.as_bytes(), append)
    {
        outcome.record.status = Status::Error;
        outcome.reason = Some(e.clone());
        outcome.record.output_error = Some(e);
    }
    outcome.record.changed_files = before.and_then(|before| {
        let after = changes::snapshot(&dir, Some(&before))?;
        Some(before.changed(&after))
    });
    outcome
}
