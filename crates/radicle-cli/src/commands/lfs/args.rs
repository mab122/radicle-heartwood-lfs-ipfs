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
    /// Store an LFS object's content in IPFS (invoked by the custom
    /// transfer agent; not for interactive use)
    #[command(hide = true)]
    Store {
        #[arg(long)]
        oid: String,
        #[arg(long)]
        size: i64,
        path: std::path::PathBuf,
    },
    /// Fetch an LFS object's content from IPFS (invoked by the custom
    /// transfer agent; not for interactive use)
    #[command(hide = true)]
    Fetch {
        #[arg(long)]
        oid: String,
        #[arg(long)]
        size: i64,
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Grant newly-authorized collaborators access to previously-encrypted
    /// LFS objects
    Rekey,
    /// Store every staged LFS file's content in IPFS in one batch (invoked
    /// by the pre-commit hook; not for interactive use). Reads
    /// "<oid> <size> <path>" lines from stdin.
    #[command(hide = true)]
    Precommit,
    /// Retroactively pin any LFS-tracked file at HEAD that was committed
    /// without going through the pre-commit hook (e.g. `--no-verify`, or
    /// the IPFS daemon/`ipfs`/`rad` weren't available at commit time)
    Backfill,
    /// Long-lived worker that fetches many LFS objects across one process
    /// (invoked by the custom transfer agent; not for interactive use).
    /// Reads "<oid> <size> <out-path>" lines from stdin, one at a time,
    /// writing a JSON response line for each.
    #[command(hide = true)]
    FetchBatch,
}
