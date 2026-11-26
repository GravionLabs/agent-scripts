//! `changedFiles` of a run (ADR 0019 "Output"): the paths git reports as modified or untracked at
//! the end whose content differs from the start. It only reads.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};

use crate::process;

/// What git reports as changed in a working tree, with the content of each path.
#[derive(Debug)]
pub(crate) struct Snapshot {
    root: PathBuf,
    /// The content filters git is configured with: programs that `git status` starts.
    filters: Vec<u8>,
    /// By the path relative to the root, as the bytes git gives.
    paths: BTreeMap<Vec<u8>, State>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Missing,
    File([u8; 32]),
    Link(PathBuf),
    /// A directory (a submodule, a nested repository) or a file that cannot be read.
    Other,
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = git_output(dir, args)?;
    out.status.success().then_some(out.stdout)
}

fn git_output(dir: &Path, args: &[&str]) -> Option<Output> {
    let mut command = Command::new(process::resolve("git").ok()?);
    // a file system monitor of the repository config would be a program git starts
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // `dir` names the repository, not the environment of a git hook
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(name);
    }
    command.output().ok()
}

/// The `filter.*` entries of the configuration of `dir`; reading them starts nothing.
fn filters(dir: &Path) -> Option<Vec<u8>> {
    let out = git_output(dir, &["config", "-z", "--get-regexp", "^filter\\."])?;
    // 1: there is none
    matches!(out.status.code(), Some(0 | 1)).then_some(out.stdout)
}

#[cfg(unix)]
fn path_of(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).into()
}

#[cfg(not(unix))]
fn path_of(bytes: &[u8]) -> PathBuf {
    String::from_utf8_lossy(bytes).into_owned().into()
}

fn state(path: &Path) -> State {
    let Ok(meta) = path.symlink_metadata() else {
        return State::Missing;
    };
    if meta.is_symlink() {
        return std::fs::read_link(path).map_or(State::Other, State::Link);
    }
    if !meta.is_file() {
        return State::Other;
    }
    hash(path).map_or(State::Other, State::File)
}

fn hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        match file.read(&mut chunk)? {
            0 => return Ok(hash.finalize().into()),
            n => hash.update(&chunk[..n]),
        }
    }
}

/// The state of the working tree that holds `dir`; `None` outside one. With the snapshot of the
/// start as `before`, also `None` when the filters changed since: the agent may have written the
/// configuration, and its programs would run without the limits of the script.
pub(crate) fn snapshot(dir: &Path, before: Option<&Snapshot>) -> Option<Snapshot> {
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let top = top.strip_suffix(b"\n").unwrap_or(&top);
    let root = path_of(top);
    let filters = filters(dir)?;
    if before.is_some_and(|b| b.filters != filters) {
        return None;
    }
    let status = git(
        dir,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            // git would run in the submodule, with its configuration
            "--ignore-submodules=all",
        ],
    )?;
    // each entry is two letters, a space and the path
    let paths = status
        .split(|&b| b == 0)
        .filter_map(|entry| entry.get(3..))
        .map(|path| (path.to_vec(), state(&root.join(path_of(path)))))
        .collect();
    Some(Snapshot {
        root,
        filters,
        paths,
    })
}

impl Snapshot {
    /// The changed paths of the end `end` against this start, relative to the repository root.
    pub(crate) fn changed(&self, end: &Snapshot) -> Vec<String> {
        end.paths
            .iter()
            .filter(|(path, now)| {
                self.root != end.root || self.paths.get(*path).is_none_or(|before| before != *now)
            })
            .map(|(path, _)| String::from_utf8_lossy(path).into_owned())
            .collect()
    }
}
