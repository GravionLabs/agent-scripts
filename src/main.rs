use clap::Parser;

/// Run agent scripts: saved agent calls as Markdown files with YAML frontmatter.
#[derive(Debug, Parser)]
#[command(name = "agent-scripts", version = agent_scripts::VERSION, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
