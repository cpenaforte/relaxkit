use clap::Parser;

use relaxkit::cli::Cli;
use relaxkit::ui::{self, Palette};

fn main() {
    match relaxkit::run(Cli::parse()) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(err) => {
            let palette = Palette::for_stream(ui::stderr_is_terminal());
            for line in ui::error_lines(&err, palette) {
                eprintln!("{line}");
            }
            std::process::exit(1);
        }
    }
}
