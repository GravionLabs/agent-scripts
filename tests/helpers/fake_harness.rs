//! A harness for the tests of `run`, started in place of an agent CLI. It records its call and does
//! what the `FAKE_*` variables ask for:
//!
//! - `FAKE_RECORD`: writes its arguments, its working directory and `FAKE_MARK` there, one
//!   NUL-terminated value each.
//! - `FAKE_STDOUT`, `FAKE_STDERR`: what it prints.
//! - `FAKE_WRITE`: lines `<path>=<content>`, files it writes.
//! - `FAKE_DELETE`: a file it deletes.
//! - `FAKE_CHILD`: a file; it starts a child of its own that appends to that file while it lives.
//!   `FAKE_CHILD_STUBBORN` makes the child ignore SIGTERM, `FAKE_CHILD_SESSION` gives it a session
//!   of its own. The harness continues once the child lives.
//! - `FAKE_STARTED`: a file it creates with its process id when it is ready.
//! - `FAKE_SLEEP_MS`: how long it stays.
//! - `FAKE_EXIT`: its exit code.

use std::env::var;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A forgotten child ends by itself.
const CHILD_LIFE: Duration = Duration::from_secs(60);

fn main() {
    if let Ok(beat) = var("FAKE_BEAT") {
        child(&beat);
        return;
    }
    if let Ok(record) = var("FAKE_RECORD") {
        let mut values: Vec<String> = std::env::args().skip(1).collect();
        values.push(std::env::current_dir().expect("cwd").display().to_string());
        values.push(var("FAKE_MARK").unwrap_or_default());
        let text: String = values.iter().map(|v| format!("{v}\0")).collect();
        fs::write(record, text).expect("record");
    }
    if let Ok(files) = var("FAKE_WRITE") {
        for line in files.lines() {
            let (path, content) = line.split_once('=').expect("path=content");
            fs::write(path, content).expect("write");
        }
    }
    if let Ok(path) = var("FAKE_DELETE") {
        fs::remove_file(path).expect("delete");
    }
    if let Ok(beat) = var("FAKE_CHILD") {
        // it keeps the pipes of the harness, as a command of an agent would
        // the harness ends while its child lives: that is what the runner has to clean up
        #[allow(clippy::zombie_processes)]
        Command::new(std::env::current_exe().expect("own path"))
            .env("FAKE_BEAT", &beat)
            .stdin(Stdio::null())
            .spawn()
            .expect("child");
        while fs::metadata(&beat).map_or(true, |m| m.len() == 0) {
            std::thread::sleep(Duration::from_millis(10));
        }
        // long enough for a runner to see the child below this process
        std::thread::sleep(Duration::from_millis(800));
    }
    print!("{}", var("FAKE_STDOUT").unwrap_or_default());
    eprint!("{}", var("FAKE_STDERR").unwrap_or_default());
    std::io::stdout().flush().expect("flush");
    if let Ok(started) = var("FAKE_STARTED") {
        // complete once it is there
        let part = format!("{started}.part");
        fs::write(&part, std::process::id().to_string()).expect("started");
        fs::rename(part, started).expect("started");
    }
    if let Ok(ms) = var("FAKE_SLEEP_MS") {
        std::thread::sleep(Duration::from_millis(ms.parse().expect("milliseconds")));
    }
    let code = var("FAKE_EXIT").map_or(0, |c| c.parse().expect("exit code"));
    std::process::exit(code);
}

fn child(beat: &str) {
    #[cfg(unix)]
    // the signal and session calls have no safe form in std
    #[allow(unsafe_code)]
    // SAFETY: both calls touch no memory.
    unsafe {
        if var("FAKE_CHILD_STUBBORN").is_ok() {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
        if var("FAKE_CHILD_SESSION").is_ok() {
            libc::setsid();
        }
    }
    let started = Instant::now();
    while started.elapsed() < CHILD_LIFE {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(beat)
            .expect("beat");
        let _ = file.write_all(b".");
        std::thread::sleep(Duration::from_millis(20));
    }
}
