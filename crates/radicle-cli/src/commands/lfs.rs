//! `rad lfs` command implementation.

pub mod init;

mod args;

use crate::terminal as term;

pub use args::Args;
use args::Command;

pub fn run(args: Args, ctx: impl term::Context) -> anyhow::Result<()> {
    // No subcommand currently needs a `Profile`, but we take `ctx` to stay
    // consistent with the other top-level commands' `run` signature.
    let _ = ctx;

    match args.command {
        Command::Init => self::init::run(),
    }
}
