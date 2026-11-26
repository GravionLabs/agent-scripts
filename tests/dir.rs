//! Directories of scripts, in temporary directories.

use std::fs;
use std::path::Path;

use agent_scripts::dir::{self, Source};

fn write(dir: &Path, file: &str, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(file), text).unwrap();
}

fn script(front: &str, prompt: &str) -> String {
    format!("---\n{front}\n---\n{prompt}\n")
}

#[test]
fn a_project_script_replaces_the_user_script_of_the_same_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (project, user) = (tmp.path().join("project"), tmp.path().join("user"));
    write(&project, "b.md", &script("{}", "project b"));
    write(&project, "other-file.md", &script("name: c", "project c"));
    write(&user, "a.md", &script("{}", "user a"));
    write(&user, "b.md", &script("{}", "user b"));
    write(&user, "c.md", &script("{}", "user c"));

    let list = dir::list(Some(&project), Some(&user)).unwrap();
    let rows: Vec<(&str, Source, &str)> = list
        .iter()
        .map(|e| {
            let s = e.script.as_ref().unwrap();
            (e.name(), e.source, s.prompt.as_str())
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("a", Source::User, "user a"),
            ("b", Source::Project, "project b"),
            ("c", Source::Project, "project c"),
        ]
    );
}

#[test]
fn an_invalid_project_script_still_replaces_the_user_script() {
    let tmp = tempfile::tempdir().unwrap();
    let (project, user) = (tmp.path().join("project"), tmp.path().join("user"));
    write(&project, "a.md", "no frontmatter");
    write(&user, "a.md", &script("{}", "user a"));

    let list = dir::list(Some(&project), Some(&user)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].source, Source::Project);
    assert!(list[0].script.is_err());
}

#[test]
fn only_md_files_of_one_level_are_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    let scripts = tmp.path();
    write(scripts, "a.md", &script("{}", "a"));
    write(scripts, "notes.txt", "text");
    write(&scripts.join("sub"), "b.md", &script("{}", "b"));
    fs::create_dir(scripts.join("folder.md")).unwrap();

    let list = dir::scan(scripts).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].as_ref().unwrap().name, "a");
}

#[test]
fn a_missing_directory_has_no_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    assert!(
        dir::list(Some(&missing), Some(&missing))
            .unwrap()
            .is_empty()
    );
    assert!(dir::list(None, None).unwrap().is_empty());
}

#[test]
fn a_directory_that_cannot_be_listed_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    fs::write(&file, "x").unwrap();
    assert!(dir::list(Some(&file), None).is_err());
}

#[test]
fn both_scripts_of_a_duplicate_name_are_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    // the name of one comes from its file name, the other sets it, and is broken in another way
    write(tmp.path(), "a.md", &script("{}", "first"));
    write(
        tmp.path(),
        "z.md",
        &script("name: a\ntools: Read", "second"),
    );
    write(tmp.path(), "ok.md", &script("{}", "ok"));

    let list = dir::scan(tmp.path()).unwrap();
    let errors: Vec<Vec<&str>> = list
        .iter()
        .map(|s| match s {
            Ok(_) => Vec::new(),
            Err(i) => i.errors.iter().map(|e| e.key.as_str()).collect(),
        })
        .collect();
    assert_eq!(errors, [vec!["name"], vec![], vec!["tools", "name"]]);

    // loading one file alone sees the other one too
    assert!(dir::load(&tmp.path().join("a.md")).unwrap().is_err());
    assert!(dir::load(&tmp.path().join("ok.md")).unwrap().is_ok());
}

#[test]
fn a_stand_in_name_makes_no_script_a_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "deploy.md",
        &script("name: My Deploy", "bad name"),
    );
    write(tmp.path(), "ship.md", "no frontmatter");
    write(tmp.path(), "b.md", &script("name: deploy", "b"));
    write(tmp.path(), "c.md", &script("name: ship", "c"));

    for file in ["b.md", "c.md"] {
        assert!(dir::load(&tmp.path().join(file)).unwrap().is_ok(), "{file}");
    }
}

#[test]
fn a_script_of_an_unknown_format_keeps_its_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (project, user) = (tmp.path().join("project"), tmp.path().join("user"));
    write(
        &project,
        "foo.md",
        &script("format: 2\nname: deploy", "new"),
    );
    write(&user, "deploy.md", &script("{}", "user"));

    let list = dir::list(Some(&project), Some(&user)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].name(), list[0].source),
        ("deploy", Source::Project)
    );
    assert!(list[0].script.is_err());

    // and it is a duplicate of a script with that name in its own directory
    write(&project, "a.md", &script("name: deploy", "a"));
    assert!(dir::load(&project.join("a.md")).unwrap().is_err());
}

#[test]
fn a_file_that_is_not_utf8_is_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.md");
    fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
    let invalid = dir::load(&file).unwrap().unwrap_err();
    assert_eq!(invalid.name, "a");
    assert_eq!(invalid.errors[0].key, "file");
}

#[test]
fn loading_a_missing_file_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(dir::load(&tmp.path().join("missing.md")).is_err());
}

#[test]
fn the_project_root_is_the_directory_with_git() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let deep = root.join("src/deep");
    fs::create_dir_all(&deep).unwrap();
    // a worktree has a `.git` file
    fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
    assert_eq!(dir::project_root(&deep), root);
    assert_eq!(dir::project_root(&root), root);
}

#[test]
fn dot_dot_is_resolved_before_the_root_is_searched() {
    let tmp = tempfile::tempdir().unwrap();
    // the real path: macOS and Windows reach their temporary directory through links or short names
    let base = tmp.path().canonicalize().unwrap();
    let (repo, plain) = (base.join("repo"), base.join("plain"));
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&plain).unwrap();
    // pushed one by one: `join("../plain")` would keep the `/` on Windows
    let sideways = repo.join("..").join("plain");
    assert_eq!(dir::project_root(&sideways), plain);
    assert_ne!(dir::project_root(&repo.join("..")), repo);
}

#[test]
fn the_user_scripts_are_in_the_config_directory() {
    // reads only the platform's path, nothing on disk
    assert_eq!(
        dir::user_scripts_dir(),
        dirs::config_dir().map(|c| c.join("agent-scripts").join("scripts"))
    );
}
