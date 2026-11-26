//! The agent-events run of a script run (GravionLabs/agent-events: `SPEC.md` and the `jsonl-file`
//! binding): `<git-common-dir>/.agents/events/<run>.jsonl` with the start of the run, one task for
//! the script and the end of both. Outside a git working tree there is none. Nothing of the prompt
//! or of the answer is written. A failure never reaches the run: it is kept as a warning.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::changes;
use crate::format::Script;
use crate::run::Status;

/// The id of the one task of a run.
const TASK: &str = "script";
/// A longer harness or model id stays out: a line has at most 16384 bytes.
const MAX_ID: usize = 256;

/// The writer of one run file.
pub(crate) struct Events {
    /// `None` after the first failure: no line follows a lost one, so `seq` has no gap.
    file: Option<File>,
    path: PathBuf,
    run: String,
    seq: u64,
    warning: Option<String>,
}

/// `<git-common-dir>/.agents/events` of the working tree that holds `dir`.
fn directory(dir: &Path) -> Option<PathBuf> {
    let out = changes::git(
        dir,
        &["rev-parse", "--is-inside-work-tree", "--git-common-dir"],
    )?;
    let common = out.strip_prefix(b"true\n")?;
    let common = common.strip_suffix(b"\n").unwrap_or(common);
    // relative to `dir`, or absolute
    Some(
        dir.join(changes::path_of(common))
            .join(".agents")
            .join("events"),
    )
}

/// A run id of the spec: it is the name of the file, so it has no path separator.
fn is_run_id(id: &str) -> bool {
    id.len() <= 128
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The UTC time `millis` after 1970 in RFC 3339.
fn timestamp(millis: u128) -> String {
    let seconds = u64::try_from(millis / 1000).unwrap_or(u64::MAX);
    let (day, time) = (seconds / 86_400, seconds % 86_400);
    // the civil date of a day number, in years that start in March (a leap day ends them)
    let z = day + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3600,
        time / 60 % 60,
        time % 60,
        millis % 1000
    )
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// A run id no other run has: the time, the script, the process and a count within it.
fn new_run_id(name: &str) -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let time: String = timestamp(now())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let count = COUNT.fetch_add(1, Ordering::Relaxed);
    // at most 19 + 64 + 10 + 20 and the dashes
    format!("{time}-{name}-{}-{count}", std::process::id())
}

impl Events {
    /// Creates the file of a run of `script` in `dir` and writes the start of the run and of its
    /// task. `id` is the run id the caller wants. `None` outside a git working tree.
    pub(crate) fn start(dir: &Path, id: Option<&str>, script: &Script) -> Option<Events> {
        let events = directory(dir)?;
        let run = id.map_or_else(|| new_run_id(&script.name), str::to_owned);
        let path = events.join(format!("{run}.jsonl"));
        let opened = if is_run_id(&run) {
            // a file that is there belongs to another writer, and a run starts only once
            fs::create_dir_all(&events)
                .and_then(|()| File::options().append(true).create_new(true).open(&path))
                .map_err(|e| format!("{}: {e}", path.display()))
        } else {
            Err(format!("{run:?} is not a run id"))
        };
        let (file, warning) = match opened {
            Ok(file) => (Some(file), None),
            Err(e) => (None, Some(format!("no events are written: {e}"))),
        };
        let mut events = Events {
            file,
            path,
            run,
            seq: 0,
            warning,
        };
        let subject = json!({
            "kind": "agent-scripts.script",
            "id": script.name,
            "title": script.name,
        });
        let mut agent = serde_json::Map::new();
        let ids = [
            ("harness", Some(&script.harness)),
            ("model", script.model.as_ref()),
        ];
        for (key, value) in ids {
            if let Some(value) = value.filter(|v| v.len() <= MAX_ID) {
                agent.insert(key.into(), json!(value));
            }
        }
        events.write(
            "run.started",
            "info",
            json!({
                "profiles": [],
                "host": { "jsonl-file.pid": std::process::id().to_string() },
                "agent": agent,
                "subject": subject,
            }),
        );
        events.write(
            "task.started",
            "info",
            json!({ "task": TASK, "subject": subject }),
        );
        Some(events)
    }

    /// Writes the end of the task and of the run; returns the warning of the run's events.
    pub(crate) fn finish(mut self, status: Status) -> Option<String> {
        let (outcome, reason, tone) = match status {
            Status::Ok => ("succeeded", "done", "success"),
            Status::Cancelled => ("cancelled", status.as_str(), "warning"),
            Status::Error | Status::Failed | Status::Timeout | Status::Refused => {
                ("failed", status.as_str(), "danger")
            }
        };
        let end = json!({ "outcome": outcome, "reason": reason });
        let mut task = end.clone();
        task["task"] = json!(TASK);
        self.write("task.finished", tone, task);
        self.write("run.finished", tone, end);
        self.warning
    }

    /// Appends one event as one line in a single write.
    fn write(&mut self, kind: &str, tone: &str, mut event: Value) {
        let Some(file) = &mut self.file else { return };
        self.seq += 1;
        event["v"] = json!(1);
        event["seq"] = json!(self.seq);
        event["ts"] = json!(timestamp(now()));
        event["run"] = json!(self.run);
        event["type"] = json!(kind);
        event["tone"] = json!(tone);
        if let Err(e) = file.write_all(format!("{event}\n").as_bytes()) {
            self.file = None;
            self.warning = Some(format!(
                "the events stop here: {}: {e}",
                self.path.display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_rfc_3339() {
        for (millis, text) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_399_999, "2000-02-28T23:59:59.999Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_709_251_200_001, "2024-03-01T00:00:00.001Z"),
            (1_798_761_599_000, "2026-12-31T23:59:59.000Z"),
            (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
        ] {
            assert_eq!(timestamp(millis), text);
        }
    }

    #[test]
    fn run_ids() {
        let longest = new_run_id(&"a".repeat(crate::format::MAX_NAME_LEN));
        assert!(is_run_id(&longest), "{longest}");
        assert_ne!(new_run_id("a"), new_run_id("a"));
        for id in ["", ".a", "-a", "a/b", "a\\b", "a b", &"a".repeat(129)] {
            assert!(!is_run_id(id), "{id:?}");
        }
    }

    #[test]
    fn nothing_follows_a_failed_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("r.jsonl");
        fs::write(&path, "").unwrap();
        let mut events = Events {
            // a file that is open for reading only
            file: Some(File::open(&path).unwrap()),
            path: path.clone(),
            run: "r".into(),
            seq: 0,
            warning: None,
        };
        events.write("progress", "info", json!({}));
        assert!(events.file.is_none());
        let warning = events.finish(Status::Ok).expect("a warning");
        assert!(warning.contains("r.jsonl"), "{warning}");
        assert_eq!(fs::read_to_string(path).unwrap(), "");
    }
}
