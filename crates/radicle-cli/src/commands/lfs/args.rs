use clap::{Parser, Subcommand};

const ABOUT: &str = "Manage Git LFS (large file) support, backed by IPFS";

const LONG_ABOUT: &str = r#"
The `lfs` command manages Git LFS (Large File Storage) support for a
Radicle repository. Large file content is stored on the contributor's
local IPFS (Kubo) node rather than on a seed-hosted HTTP server: there
is no additional server-side infrastructure to run.

See `rad lfs init --help` for the one-time setup command.
"#;

#[derive(Parser, Debug)]
#[command(about = ABOUT, long_about = LONG_ABOUT, disable_version_flag = true)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Set up Git LFS support for this repository, backed by IPFS
    Init,
}
