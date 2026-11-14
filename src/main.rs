use std::process::ExitCode;

use clap::Parser;
use korg::cli::{Cli, execute};

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}
