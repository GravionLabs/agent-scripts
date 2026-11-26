//! The output targets that write (ADR 0019 "Output"): `file` and `append`. Their path is relative
//! to the directory of the run and never leaves it, also not through a symlink.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::format::path_stays_inside;

/// The file of the expanded output path `path` below `dir`, judged by its text. `path` may start
/// with `dir` itself (`{dir}/out.md`). Its `..` are resolved here: what is checked later is what is
/// opened, whether the directory before a `..` exists or not.
pub(crate) fn resolve(dir: &Path, path: &str) -> Result<PathBuf, String> {
    let dir_text = dir.to_string_lossy();
    let relative = path
        .strip_prefix(dir_text.as_ref())
        .and_then(|rest| rest.strip_prefix(['/', std::path::MAIN_SEPARATOR]))
        .map_or(path, |rest| rest.trim_start_matches(['/', '\\']));
    let outside =
        || format!("the output path {path:?} does not name a file inside the directory of the run");
    if !path_stays_inside(relative) {
        return Err(outside());
    }
    let mut below = PathBuf::new();
    for part in Path::new(relative).components() {
        match part {
            Component::Normal(name) => below.push(name),
            Component::CurDir => {}
            Component::ParentDir if below.pop() => {}
            _ => return Err(outside()),
        }
    }
    if below.as_os_str().is_empty() {
        return Err(outside());
    }
    Ok(dir.join(below))
}

/// Checks that what exists of `file` lies inside `dir`.
fn contained(dir: &Path, file: &Path) -> Result<(), String> {
    let root = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let existing = file
        .ancestors()
        .find(|a| a.symlink_metadata().is_ok())
        .unwrap_or(dir);
    let real = existing
        .canonicalize()
        .map_err(|e| format!("{}: {e}", existing.display()))?;
    if !real.starts_with(&root) {
        return Err(format!(
            "{}: leads out of the directory of the run",
            existing.display()
        ));
    }
    Ok(())
}

/// Writes `text` to `file` below `dir`; with `append` after what is there, else only to a new file.
pub(crate) fn write(dir: &Path, file: &Path, text: &[u8], append: bool) -> Result<(), String> {
    let fail = |e: io::Error| match e.kind() {
        io::ErrorKind::AlreadyExists if !append => {
            format!("{}: exists already, nothing is overwritten", file.display())
        }
        _ => format!("{}: {e}", file.display()),
    };
    // an existing target of `file` is refused by `create_new`, which follows no symlink
    if append || file.symlink_metadata().is_err() {
        contained(dir, file)?;
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    if append {
        options.append(true).create(true);
    } else {
        options.write(true).create_new(true);
    }
    options
        .open(file)
        .and_then(|mut f| f.write_all(text))
        .map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_stays_in_the_directory() {
        let dir = Path::new(if cfg!(windows) { "C:\\w" } else { "/w" });
        let text = dir.to_str().unwrap();
        assert_eq!(resolve(dir, "a/b"), Ok(dir.join("a/b")));
        assert_eq!(resolve(dir, "no/../a/./b"), Ok(dir.join("a/b")));
        assert_eq!(resolve(dir, &format!("{text}/a")), Ok(dir.join("a")));
        for path in [
            "../a".to_owned(),
            format!("{text}/../a"),
            format!("{text}x/a"),
            text.to_owned(),
            "/etc/passwd".to_owned(),
        ] {
            assert!(resolve(dir, &path).is_err(), "{path}");
        }
    }
}
