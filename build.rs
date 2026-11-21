// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::cli::Cli;
use clap::{CommandFactory, ValueEnum};
use clap_complete_command::Shell;
use remove_dir_all::ensure_empty_dir;
use std::io;

#[path = "src/cli.rs"]
mod cli;

fn main() -> Result<(), io::Error> {
    let out_dir = build_rs::input::out_dir();
    let completions = out_dir.join("completions");
    let man = out_dir.join("man");
    ensure_empty_dir(&completions)?;
    ensure_empty_dir(&man)?;
    for variant in Shell::value_variants() {
        variant.generate_to(&mut Cli::command(), &completions)?;
    }
    clap_mangen::generate_to(Cli::command(), &man)
}
