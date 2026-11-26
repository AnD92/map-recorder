use std::process::ExitCode;

use clap::Parser;
use map_recorder::cli::{Cli, run};

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
