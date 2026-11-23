//! The `nanoflash` command.

#![forbid(unsafe_code)]

use clap::{CommandFactory, Parser};

/// Lay out flash and write firmware to microcontroller boards of any vendor.
#[derive(Debug, Parser)]
#[command(name = "nanoflash", version, about)]
struct Arguments {}

fn main() {
    Arguments::parse();
    // No subcommand exists yet: show the help.
    let _ = Arguments::command().print_help();
}
