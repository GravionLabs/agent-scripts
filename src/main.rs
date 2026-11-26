use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use agent_scripts::dir::{self, Entry, Loaded};
use agent_scripts::format::Issue;
use agent_scripts::run::{self, Host, Request, Status};
use clap::{Args, Parser, Subcommand};

/// Run agent scripts: saved agent calls as Markdown files with YAML frontmatter.
#[derive(Debug, Parser)]
#[command(name = "agent-scripts", version = agent_scripts::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a script with its harness in print mode.
    ///
    /// Exit codes: 0 ok, 2 usage error, 3 the harness could not be started, 4 error, 5 timeout,
    /// 6 refused, 130 and 143 cancelled by SIGINT and SIGTERM.
    Run(RunArgs),
    /// Check a script file; exit code 2 with the reasons when it is invalid.
    Validate {
        /// The script file.
        path: PathBuf,
    },
    /// List the project and user scripts.
    List {
        /// A directory of the project [default: the current directory].
        dir: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct RunArgs {
    /// A script file, or the name of a project or user script.
    script: String,
    /// The working directory of the run [default: the current directory].
    #[arg(long)]
    dir: Option<PathBuf>,
    /// A value of a declared input; may be given more than once.
    #[arg(long = "input", value_name = "K=V", value_parser = input)]
    inputs: Vec<(String, String)>,
    /// The value of {file}.
    #[arg(long)]
    file: Option<String>,
    /// The value of {selection}.
    #[arg(long, allow_hyphen_values = true, conflicts_with = "selection_file")]
    selection: Option<String>,
    /// A file whose content is the value of {selection}.
    #[arg(long)]
    selection_file: Option<PathBuf>,
    /// Print the result record as JSON instead of the text.
    #[arg(long)]
    json: bool,
    /// Refuse the script unless the SHA-256 of its content is this hex value.
    #[arg(long, value_name = "HASH")]
    expect_sha256: Option<String>,
}

fn input(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| "expected <name>=<value>".to_owned())
}

/// Usage error or invalid script.
const INVALID: u8 = 2;
const FAILED: u8 = 3;
const ERROR: u8 = 4;
const TIMEOUT: u8 = 5;
const REFUSED: u8 = 6;

fn main() -> ExitCode {
    match Cli::parse().command {
        Some(Command::Run(args)) => run_script(args),
        Some(Command::Validate { path }) => validate(&path),
        Some(Command::List { dir }) => list(dir.as_deref().unwrap_or(Path::new("."))),
        None => ExitCode::SUCCESS,
    }
}

/// Text of a file may come from a repository: keep it on one line and free of terminal escapes.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A line on stdout; a closed pipe (`list | head`) is not an error.
fn out(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn report(file: &Path, kind: &str, issues: &[Issue]) {
    for issue in issues {
        let line = format!("{}: {kind}{issue}", file.display());
        let _ = writeln!(std::io::stderr(), "{}", clean(&line));
    }
}

fn warn(loaded: &Loaded) {
    let (file, warnings) = match loaded {
        Ok(s) => (&s.file, &s.warnings),
        Err(i) => (&i.file, &i.warnings),
    };
    report(file, "warning: ", warnings);
}

fn fail(message: String) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "error: {}", clean(&message));
    ExitCode::from(INVALID)
}

fn validate(path: &Path) -> ExitCode {
    let loaded = match dir::load(path) {
        Ok(loaded) => loaded,
        Err(e) => return fail(format!("{}: {e}", path.display())),
    };
    warn(&loaded);
    match loaded {
        Ok(_) => {
            out(&clean(&format!("{}: valid", path.display())));
            ExitCode::SUCCESS
        }
        Err(invalid) => {
            report(&invalid.file, "", &invalid.errors);
            ExitCode::from(INVALID)
        }
    }
}

fn list(start: &Path) -> ExitCode {
    if !start.is_dir() {
        return fail(format!("{}: not a directory", start.display()));
    }
    let project = dir::project_root(start).join(dir::PROJECT_SCRIPTS);
    let entries = match dir::list(Some(&project), dir::user_scripts_dir().as_deref()) {
        Ok(entries) => entries,
        Err(e) => return fail(e.to_string()),
    };
    entries.iter().for_each(|e| warn(&e.script));
    print_table(&entries);
    ExitCode::SUCCESS
}

/// Columns: name, source, harness, then `valid` or the reasons.
fn print_table(entries: &[Entry]) {
    let rows: Vec<[String; 4]> = entries
        .iter()
        .map(|e| {
            let (harness, state) = match &e.script {
                Ok(s) => (s.harness.clone(), "valid".to_owned()),
                Err(i) => {
                    let reasons: Vec<String> = i.errors.iter().map(Issue::to_string).collect();
                    (
                        i.harness.clone().unwrap_or_else(|| "-".into()),
                        format!("invalid: {}", reasons.join("; ")),
                    )
                }
            };
            [e.name(), e.source.as_str(), &harness, &state].map(clean)
        })
        .collect();
    let width = |col: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (name, source, harness) = (width(0), width(1), width(2));
    for r in &rows {
        out(&format!(
            "{:name$}  {:source$}  {:harness$}  {}",
            r[0], r[1], r[2], r[3]
        ));
    }
}

/// The number of the signal that cancelled the run, 0 for none.
#[derive(Default)]
struct Cancel {
    flag: Arc<AtomicBool>,
    signal: Arc<AtomicUsize>,
}

/// SIGINT and SIGTERM cancel the run. So do SIGHUP and SIGQUIT, unless they are ignored (`nohup`):
/// the harness has a session of its own and would go on without the runner.
#[cfg(unix)]
fn on_signals() -> std::io::Result<Cancel> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::flag;

    let cancel = Cancel::default();
    for signal in [SIGINT, SIGTERM, SIGHUP, SIGQUIT] {
        if matches!(signal, SIGHUP | SIGQUIT) && ignored(signal) {
            continue;
        }
        // in this order: the number is there when the flag is seen
        flag::register_usize(signal, cancel.signal.clone(), signal as usize)?;
        flag::register(signal, cancel.flag.clone())?;
    }
    Ok(cancel)
}

#[cfg(unix)]
// reading the action of a signal has no safe form in std
#[allow(unsafe_code)]
fn ignored(signal: i32) -> bool {
    // SAFETY: a null new action only reads the current one into a struct of the right type.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut action) == 0
            && action.sa_sigaction == libc::SIG_IGN
    }
}

/// Ctrl-C and Ctrl-Break cancel the run.
#[cfg(windows)]
fn on_signals() -> std::io::Result<Cancel> {
    let cancel = Cancel::default();
    let (flag, signal) = (cancel.flag.clone(), cancel.signal.clone());
    ctrlc::set_handler(move || {
        // 2 is SIGINT: both end with exit code 130
        signal.store(2, Ordering::SeqCst);
        flag.store(true, Ordering::SeqCst);
    })
    .map_err(std::io::Error::other)?;
    Ok(cancel)
}

/// Text of an agent may try to steer the terminal: only lines and tabs pass to one.
fn for_terminal(text: &str, terminal: bool) -> String {
    if !terminal {
        return text.to_owned();
    }
    text.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}

fn run_script(args: RunArgs) -> ExitCode {
    let selection = match &args.selection_file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => Some(text),
            Err(e) => return fail(format!("{}: {e}", file.display())),
        },
        None => args.selection,
    };
    let request = Request {
        script: args.script,
        dir: args.dir.unwrap_or_else(|| PathBuf::from(".")),
        file: args.file,
        selection,
        inputs: args.inputs.into_iter().collect(),
        expect_sha256: args.expect_sha256,
    };
    let cancel = match on_signals() {
        Ok(cancel) => cancel,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: cannot handle signals: {e}");
            return ExitCode::from(FAILED);
        }
    };
    let outcome = run::run(&request, &Host::user(), &cancel.flag);

    for warning in &outcome.warnings {
        let line = format!("{}: warning: {warning}", request.script);
        let _ = writeln!(std::io::stderr(), "{}", clean(&line));
    }
    let mut stderr = std::io::stderr();
    let harness = for_terminal(&outcome.stderr, stderr.is_terminal());
    let _ = stderr.write_all(harness.as_bytes());
    if !harness.is_empty() && !harness.ends_with('\n') {
        let _ = writeln!(stderr);
    }
    let record = &outcome.record;
    let mut stdout = std::io::stdout();
    if args.json {
        out(&record.to_json().to_string());
    } else {
        let terminal = stdout.is_terminal();
        let text = for_terminal(&record.text, terminal);
        let _ = stdout.write_all(text.as_bytes());
        if terminal && !text.is_empty() && !text.ends_with('\n') {
            let _ = writeln!(stdout);
        }
    }
    let _ = stdout.flush();
    if record.status != Status::Ok {
        let status = record.status.as_str();
        let line = match &outcome.reason {
            Some(reason) => format!("{status}: {reason}"),
            None => status.to_owned(),
        };
        let _ = writeln!(stderr, "{}", clean(&line));
    }
    ExitCode::from(match record.status {
        Status::Ok => 0,
        Status::Failed => FAILED,
        Status::Error => ERROR,
        Status::Timeout => TIMEOUT,
        Status::Refused if outcome.usage => INVALID,
        Status::Refused => REFUSED,
        // 128 and the number of the signal
        Status::Cancelled => 128 + cancel.signal.load(Ordering::SeqCst) as u8,
    })
}
