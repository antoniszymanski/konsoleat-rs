// SPDX-FileCopyrightText: 2026 Antoni Szymański
// SPDX-License-Identifier: MPL-2.0

use clap::{Parser, ValueHint};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Activate or create a Konsole terminal session in a specified working directory",
    long_about = None
)]
pub struct Cli {
    #[arg(default_value = ".")]
    #[arg(value_hint = ValueHint::DirPath)]
    pub workdir: PathBuf,
}
