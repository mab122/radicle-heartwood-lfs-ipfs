//! `rad lfs backfill` — retroactively stores any already-committed
//! LFS-tracked file that doesn't yet have a `refs/notes/rad-lfs` note (so
//! it was never pinned to IPFS). This happens whenever the pre-commit hook
//! didn't run or didn't have what it needed at commit time: a commit made
//! with `--no-verify`, or one made while the `ipfs`/`rad` binaries weren't
//! on `PATH` yet (the hook soft-skips pinning in that case rather than
//! blocking the commit -- see `HOOK_BODY`'s `command -v` checks).
//!
//! Content actually needs to be available locally to backfill it from:
//! this reads from Git LFS's own local object cache
//! (`.git/lfs/objects/<oid prefix>/<oid>`, populated by the `git-lfs`
//! clean filter on `git add`, which -- unlike the pre-commit *hook* --
//! isn't skipped by `--no-verify`), falling back to the working-tree file
//! if that's missing. A file whose content isn't available through either
//! path can't be backfilled from this machine; some other peer that
//! already has it needs to run this instead.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use radicle::storage::{ReadRepository as _, ReadStorage as _};

use crate::ipfs;
use crate::terminal as term;

use super::store::store_object;

pub fn run(ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs backfill` must be run inside a Radicle repository working copy")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("`rad lfs backfill` needs a git working copy, not a bare repository"))?;
    let doc = profile.storage.repository(rid)?.identity_doc()?.doc;

    ipfs::check_daemon()?;

    // Every path git currently tracks at HEAD (not the working tree or
    // index, which may have unrelated local modifications) -- mirrors
    // HOOK_BODY's own approach of shelling out to `git` for this rather
    // than reimplementing tree/attribute logic against git2 directly.
    let ls_files = radicle::git::run(Some(workdir), ["ls-tree", "-r", "-z", "--name-only", "HEAD"])
        .context("failed to list files at HEAD")?;
    let paths: Vec<String> = String::from_utf8_lossy(&ls_files.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let mut candidates = Vec::new();
    for path in paths {
        // Matches HOOK_BODY's own check: only files the *current*
        // `.gitattributes` marks as `filter=lfs` are our concern.
        let is_lfs = radicle::git::run(Some(workdir), ["check-attr", "filter", "--", &path])
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains(": filter: lfs"))
            .unwrap_or(false);
        if !is_lfs {
            continue;
        }
        let blob_oid_out = radicle::git::run(Some(workdir), ["rev-parse", &format!("HEAD:{path}")])
            .with_context(|| format!("failed to resolve blob oid for `{path}`"))?;
        let blob_oid_str = String::from_utf8_lossy(&blob_oid_out.stdout).trim().to_string();
        let Ok(blob_oid) = radicle::git::raw::Oid::from_str(&blob_oid_str) else {
            continue;
        };
        candidates.push((path, blob_oid));
    }

    if candidates.is_empty() {
        term::success!("No LFS-tracked files found in HEAD; nothing to backfill");
        return Ok(());
    }

    let mut signer = None;
    let mut backfilled = 0;
    let mut already_pinned = 0;
    let mut unavailable = Vec::new();

    for (path, blob_oid) in candidates {
        if !crate::lfs_crypto::find_cids(&repo, blob_oid)?.is_empty() {
            already_pinned += 1;
            continue;
        }

        let blob = repo
            .find_blob(blob_oid)
            .with_context(|| format!("failed to read blob for `{path}`"))?;
        let Ok(pointer_text) = std::str::from_utf8(blob.content()) else {
            continue;
        };
        let Some(oid) = pointer_text
            .lines()
            .find_map(|line| line.strip_prefix("oid sha256:"))
        else {
            continue;
        };
        let Some(size) = pointer_text
            .lines()
            .find_map(|line| line.strip_prefix("size "))
            .and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };

        let Some(content_path) = locate_content(workdir, oid, size, &path) else {
            unavailable.push(path);
            continue;
        };

        store_object(&profile, &repo, rid, &doc, oid, size, &content_path, &mut signer)
            .with_context(|| format!("failed to backfill `{path}`"))?;
        term::success!("Backfilled `{path}`");
        backfilled += 1;
    }

    term::blank();
    if backfilled > 0 {
        term::success!("Backfilled {backfilled} object(s)");
    }
    if already_pinned > 0 {
        term::info!("{already_pinned} object(s) already had a note; left untouched");
    }
    if !unavailable.is_empty() {
        term::warning(format!(
            "{} object(s) have no note and no content available locally to backfill from -- \
             ask a peer that already has them to run `rad lfs backfill`:",
            unavailable.len()
        ));
        for path in &unavailable {
            term::warning(format!("  {path}"));
        }
    }

    Ok(())
}

/// Git LFS's own local object cache is the reliable source: the
/// `git-lfs` clean filter populates it on `git add` regardless of
/// whether the pre-commit *hook* ran (`--no-verify` skips hooks, not
/// filters). Falls back to the working-tree file, in case content is
/// present there but the LFS cache was pruned -- rejected if the size on
/// disk doesn't match what the pointer recorded, since that means it's
/// just the unsmudged pointer text, not the real content.
fn locate_content(workdir: &Path, oid: &str, size: i64, path: &str) -> Option<PathBuf> {
    if oid.len() >= 4 {
        let cached = workdir
            .join(".git/lfs/objects")
            .join(&oid[0..2])
            .join(&oid[2..4])
            .join(oid);
        if matches_size(&cached, size) {
            return Some(cached);
        }
    }

    let working_copy = workdir.join(path);
    if matches_size(&working_copy, size) {
        return Some(working_copy);
    }

    None
}

fn matches_size(path: &Path, expected: i64) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.len() == expected as u64)
        .unwrap_or(false)
}
