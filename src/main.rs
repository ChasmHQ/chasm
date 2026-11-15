#![allow(non_snake_case)]
mod compiler;
mod config;
mod anvil;
mod debugger;
mod desktop;

use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "chasm", about = "Chasm desktop workbench for Solidity contracts")]
pub struct Cli {
    #[arg(value_name = "path", default_value = ".")]
    path: PathBuf,
    /// Explicit local Python interpreter with chasm-debugger installed (never searches PATH).
    #[arg(long)]
    debugger_python: Option<PathBuf>,
    /// Trusted local offline capture exposed as the synthetic example.
    #[arg(long)]
    debugger_example: Option<PathBuf>,
    /// Debug builds only: enable local AI access at startup, pair one Full-tier
    /// client and write the endpoint and token (one per line) to this file. The file is deleted on exit.
    #[cfg(debug_assertions)]
    #[arg(long, value_name = "file")]
    control_token_file: Option<PathBuf>,
}

fn main() {
    let matches = Cli::command().get_matches();
    let explicit_path = matches.value_source("path") == Some(clap::parser::ValueSource::CommandLine);
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
    if let Err(error) = desktop::launch(args, explicit_path) {
        desktop::show_startup_error(&format!("{error:#}"));
        eprintln!("Chasm desktop failed: {error:#}");
        std::process::exit(1);
    }
}
