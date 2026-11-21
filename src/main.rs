use clap::Parser;

use relaxkit::cli::Cli;

fn main() -> anyhow::Result<()> {
    relaxkit::run(Cli::parse())
}
