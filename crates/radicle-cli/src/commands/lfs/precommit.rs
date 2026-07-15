//! `rad lfs precommit` — plumbing command invoked once per commit by the
//! `rad-lfs` pre-commit hook. Reads every staged LFS file's oid/size/path
//! from stdin and stores them all in a single process, so a private repo's
//! passphrase (needed for the ECDH key-agreement `rad lfs store` performs)
//! is prompted for at most once per commit, not once per file.

use std::io::BufRead as _;
use std::path::PathBuf;

use anyhow::Context as _;

use radicle::storage::{ReadRepository as _, ReadStorage as _};

use crate::terminal as term;

use super::store::store_object;

pub fn run(ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs precommit` must be run inside a Radicle repository working copy")?;
    let doc = profile.storage.repository(rid)?.identity_doc()?.doc;

    let mut signer = None;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdin")?;
        // Each line is "<oid> <size> <path>" -- oid is a fixed-length hex
        // string and size is decimal, so splitting on the first two spaces
        // leaves the path (which may itself contain spaces) intact as the
        // remainder.
        let mut parts = line.splitn(3, ' ');
        let (Some(oid), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            anyhow::bail!("malformed `rad lfs precommit` input line: {line:?}");
        };
        let size: i64 = size
            .parse()
            .with_context(|| format!("invalid size in `rad lfs precommit` input: {size:?}"))?;

        store_object(
            &profile,
            &repo,
            rid,
            &doc,
            oid,
            size,
            &PathBuf::from(path),
            &mut signer,
        )
        .with_context(|| format!("failed to store LFS object for `{path}`"))?;
    }

    Ok(())
}
