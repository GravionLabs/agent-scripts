//! Agent scripts: saved agent calls as Markdown files with YAML frontmatter, and the runner that
//! executes them (ADR 0019 of GravionLabs/poimen).
//!
//! - [`format`](mod@format): parses and validates the text of one script file.
//! - [`expand`](mod@expand): fills the variables of a prompt.
//! - [`dir`]: loads script files and lists the project and user scripts.
//! - [`harness`]: the harness definitions and the argv of a call.
//! - [`run`](mod@run): runs a script with its harness in print mode.

pub mod dir;
pub mod expand;
pub mod format;
pub mod harness;
pub mod run;

mod changes;
mod events;
mod file_id;
mod output;
mod process;

/// The version of the crate, as written in `Cargo.toml` (the release pipeline sets it).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_semver() {
        let parts: Vec<&str> = VERSION.split(['.', '-', '+']).collect();
        assert!(
            parts.len() >= 3,
            "version {VERSION} has no major.minor.patch"
        );
        for part in &parts[..3] {
            part.parse::<u64>()
                .unwrap_or_else(|_| panic!("version {VERSION}: {part} is not a number"));
        }
    }
}
