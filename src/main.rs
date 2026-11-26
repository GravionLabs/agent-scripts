use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use agent_scripts::dir::{self, Entry, Loaded};
use agent_scripts::format::Issue;
use clap::{Parser, Subcommand};

/// Run agent scripts: saved agent calls as Markdown files with YAML frontmatter.
#[derive(Debug, Parser)]
#[command(name = "agent-scripts", version = agent_scripts::VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
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

/// Usage error or invalid script.
const INVALID: u8 = 2;

fn main() -> ExitCode {
    match Cli::parse().command {
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
