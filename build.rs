// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use crate::cli::Cli;
use carapace_spec_clap::Spec as Carapace;
use clap::CommandFactory;
use clap_complete::{
    aot::{Bash, Elvish, Fish, PowerShell, Zsh},
    generate_to,
};
use clap_complete_nushell::Nushell;
use remove_dir_all::ensure_empty_dir;
use std::{io, path::PathBuf};

#[path = "src/cli.rs"]
mod cli;

fn main() -> Result<(), io::Error> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    macro_rules! generate_completions {
        ($($generator:expr),+) => {
            let out_dir = manifest_dir.join("completions");
            ensure_empty_dir(&out_dir)?;
            $(
                generate_to($generator, &mut Cli::command(), env!("CARGO_PKG_NAME"), &out_dir)?;
            )*
        };
    }
    generate_completions!(Bash, Carapace, Elvish, Fish, Nushell, PowerShell, Zsh);

    let out_dir = manifest_dir.join("man");
    ensure_empty_dir(&out_dir)?;
    clap_mangen::generate_to(Cli::command(), out_dir)
}
