use clap::Parser;

use relaxkit::cli::Cli;

fn main() -> anyhow::Result<()> {
    let code = relaxkit::run(Cli::parse())?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
