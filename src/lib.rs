//! Agent scripts: saved agent calls as Markdown files with YAML frontmatter, and the runner that
//! executes them (ADR 0019 of GravionLabs/poimen). Parsing, running and the subcommands follow in
//! later issues; for now the crate only exposes its version.

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
