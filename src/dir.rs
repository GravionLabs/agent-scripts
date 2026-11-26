//! Script files on disk (ADR 0019 "Storage and trust"): the project scripts of
//! `.agents/scripts/*.md`, the user scripts of `scripts/*.md` in the user config directory, and
//! the merge of the two.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::format::{self, Invalid, Issue, Script};

/// The project scripts, relative to the repository root.
pub const PROJECT_SCRIPTS: &str = ".agents/scripts";

/// A loaded script file: valid, or invalid with the reasons.
pub type Loaded = Result<Script, Box<Invalid>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Project,
    User,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Project => "project",
            Source::User => "user",
        }
    }
}

/// One entry of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub source: Source,
    pub script: Loaded,
}

impl Entry {
    pub fn name(&self) -> &str {
        name_of(&self.script)
    }
}

/// The name that counts for duplicates: not the stand-in of a file whose name is unknown.
fn own_name(loaded: &Loaded) -> Option<&str> {
    match loaded {
        Ok(s) => Some(&s.name),
        Err(i) => i.named.then_some(i.name.as_str()),
    }
}

fn name_of(loaded: &Loaded) -> &str {
    match loaded {
        Ok(s) => &s.name,
        Err(i) => &i.name,
    }
}

/// `scripts` in the user config directory (`dirs::config_dir()/agent-scripts`); `None` when the
/// platform gives no config directory.
pub fn user_scripts_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("agent-scripts").join("scripts"))
}

/// The root of the repository that holds `dir` (the nearest directory with `.git`), else `dir`.
pub fn project_root(dir: &Path) -> PathBuf {
    let mut absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned());
    // `ancestors` cuts components off the text, so a `..` left in it must be resolved first
    if absolute.components().any(|c| c == Component::ParentDir)
        && let Ok(resolved) = absolute.canonicalize()
    {
        absolute = resolved;
    }
    absolute
        .ancestors()
        .find(|a| a.join(".git").exists())
        .unwrap_or(&absolute)
        .to_owned()
}

/// Reads one file and checks it by itself. A file that cannot be read is invalid.
fn read(file: &Path) -> Loaded {
    match std::fs::read_to_string(file) {
        Ok(text) => format::parse(file, &text),
        Err(e) => Err(Box::new(Invalid {
            file: file.to_owned(),
            name: format::file_stem(file),
            named: false,
            harness: None,
            errors: vec![Issue::new("file", format!("cannot be read: {e}"))],
            warnings: Vec::new(),
        })),
    }
}

/// The `*.md` files of `dir`, one level, sorted; empty when `dir` does not exist.
fn script_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|x| x == "md") && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Makes every script invalid whose name another file of the same directory has too.
fn reject_duplicates(scripts: Vec<Loaded>) -> Vec<Loaded> {
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &scripts {
        let file = match s {
            Ok(s) => &s.file,
            Err(i) => &i.file,
        };
        let file = file.file_name().unwrap_or_default().to_string_lossy();
        if let Some(name) = own_name(s) {
            files
                .entry(name.to_owned())
                .or_default()
                .push(file.into_owned());
        }
    }
    files.values_mut().for_each(|f| f.sort());
    scripts
        .into_iter()
        .map(|s| {
            let Some(all) = own_name(&s).and_then(|n| files.get(n)) else {
                return s;
            };
            if all.len() < 2 {
                return s;
            }
            let error = Issue::new(
                "name",
                format!(
                    "{:?} is used by more than one script of this directory: {}",
                    name_of(&s),
                    all.join(", ")
                ),
            );
            Err(match s {
                Ok(script) => Box::new(script.into_invalid(error)),
                Err(mut invalid) => {
                    invalid.errors.push(error);
                    invalid
                }
            })
        })
        .collect()
}

/// Loads the scripts of one directory, in the order of their file names.
pub fn scan(dir: &Path) -> io::Result<Vec<Loaded>> {
    let scripts = script_files(dir)?.iter().map(|f| read(f)).collect();
    Ok(reject_duplicates(scripts))
}

/// Loads the script file `file`, including the check against the other scripts of its directory.
/// An error means that the file or its directory cannot be read.
pub fn load(file: &Path) -> io::Result<Loaded> {
    std::fs::metadata(file)?;
    let parent = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let loaded = read(file);
    // only a `*.md` file is a script of its directory
    if file.extension().is_none_or(|x| x != "md") {
        return Ok(loaded);
    }
    let own = file.file_name();
    let mut all: Vec<Loaded> = script_files(parent)?
        .iter()
        .filter(|f| f.file_name() != own)
        .map(|f| read(f))
        .collect();
    all.push(loaded);
    Ok(reject_duplicates(all)
        .pop()
        .expect("the file itself is in the list"))
}

/// The scripts of both directories, sorted by name. A project script replaces the user script of
/// the same name, also when it is invalid: it never falls back to another prompt.
pub fn list(project: Option<&Path>, user: Option<&Path>) -> io::Result<Vec<Entry>> {
    let entries = |dir: Option<&Path>, source| -> io::Result<Vec<Entry>> {
        let Some(dir) = dir else {
            return Ok(Vec::new());
        };
        let scripts =
            scan(dir).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dir.display())))?;
        Ok(scripts
            .into_iter()
            .map(|script| Entry { source, script })
            .collect())
    };
    let mut out = entries(project, Source::Project)?;
    let mut users = entries(user, Source::User)?;
    users.retain(|u| !out.iter().any(|p| p.name() == u.name()));
    out.append(&mut users);
    out.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(out)
}
