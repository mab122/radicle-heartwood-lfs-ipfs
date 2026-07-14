//! `rad lfs` command implementation.

pub mod fetch;
pub mod init;
pub mod rekey;
pub mod store;

mod args;

use crate::terminal as term;

pub use args::Args;
use args::Command;

pub fn run(args: Args, ctx: impl term::Context) -> anyhow::Result<()> {
    match args.command {
        Command::Init => self::init::run(),
        Command::Store { oid, size, path } => self::store::run(oid, size, path, ctx),
        Command::Fetch { oid, size, out } => self::fetch::run(oid, size, out, ctx),
        Command::Rekey => self::rekey::run(ctx),
    }
}
