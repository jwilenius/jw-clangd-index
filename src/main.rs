use std::process::ExitCode;

use clap::Parser;
use env_logger::{Builder, Env};
use jw_clangd_index::cli::{Cli, run};

/// Exit code of a fatal error.
const ERROR_EXIT_CODE: u8 = 2;

fn main() -> ExitCode {
    Builder::from_env(Env::default().default_filter_or("info")).format_timestamp(None).format_target(false).init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            // Printed directly so that no `RUST_LOG` setting can hide a fatal error.
            eprintln!("error: {error:#}");
            ExitCode::from(ERROR_EXIT_CODE)
        }
    }
}
