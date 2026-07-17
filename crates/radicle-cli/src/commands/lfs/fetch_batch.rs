//! `rad lfs fetch-batch` — long-lived plumbing worker invoked by the Git
//! LFS custom transfer agent (`rad-lfs-transfer`) to download multiple LFS
//! objects across a single process, kept alive for the lifetime of one
//! `git lfs pull`/checkout session -- specifically so a private repo's
//! passphrase is only prompted for once per session, not once per file.
//!
//! Unlike `rad lfs precommit`/`backfill` (which know every file upfront
//! and can just read stdin until EOF), this can't collect a batch before
//! starting: git-lfs's custom-transfer protocol requests objects one at a
//! time by default (`rad-lfs-transfer` doesn't negotiate the "concurrent"
//! capability), so the caller only knows about the next object once the
//! previous one has been answered. This process instead stays alive and
//! answers requests one at a time as they arrive, reusing the same
//! unwrapped signer across all of them.
//!
//! Protocol (line-delimited, UTF-8, on stdin/stdout):
//!   request:  "<oid> <size> <out-path>\n"
//!   response: a JSON object, one line per request, in the same order:
//!             `{"ok":true}` or `{"ok":false,"error":"<message>"}`
//! The process exits cleanly on stdin EOF (the parent closing its write
//! end signals the transfer session is over).

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

use anyhow::Context as _;
use serde_json::json;

use crate::terminal as term;

use super::fetch::fetch_object;

pub fn run(ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs fetch-batch` must be run inside a Radicle repository working copy")?;

    let mut signer = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdin")?;
        // Each line is "<oid> <size> <out-path>" -- oid is a fixed-length
        // hex string and size is decimal, so splitting on the first two
        // spaces leaves the path (which may itself contain spaces) intact
        // as the remainder. Mirrors `rad lfs precommit`'s input format.
        let mut parts = line.splitn(3, ' ');
        let (Some(oid), Some(size), Some(out)) = (parts.next(), parts.next(), parts.next())
        else {
            anyhow::bail!("malformed `rad lfs fetch-batch` input line: {line:?}");
        };
        let Ok(size) = size.parse::<i64>() else {
            anyhow::bail!("invalid size in `rad lfs fetch-batch` input: {size:?}");
        };
        let out = PathBuf::from(out);

        let response = match fetch_object(&profile, &repo, rid, oid, size, &out, &mut signer) {
            Ok(()) => json!({"ok": true}),
            Err(err) => json!({"ok": false, "error": format!("{err:#}")}),
        };
        writeln!(stdout, "{response}").context("failed to write response")?;
        stdout.flush().context("failed to flush stdout")?;
    }

    Ok(())
}
