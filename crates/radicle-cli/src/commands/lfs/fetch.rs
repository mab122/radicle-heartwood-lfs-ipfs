//! `rad lfs fetch` — plumbing command invoked by the Git LFS custom
//! transfer agent (`rad-lfs-transfer`) to download one LFS object's
//! content from the local IPFS (Kubo) node, decrypting it first if the
//! repository is private.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};

use radicle::Profile;
use radicle::git::raw::Repository;
use radicle::identity::RepoId;
use radicle_crypto::ssh::keystore::MemorySigner;

use crate::ipfs;
use crate::lfs_crypto;
use crate::terminal as term;

pub fn run(oid: String, size: i64, out: PathBuf, ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs fetch` must be run inside a Radicle repository working copy")?;

    let mut signer = None;
    fetch_object(&profile, &repo, rid, &oid, size, &out, &mut signer)
}

/// Fetches one LFS object's content from IPFS (decrypting it first for
/// private repos) and writes it to `out`. Factored out of [`run`] so
/// `rad lfs fetch-batch` can process many objects across a single
/// process, loading (and prompting for) the signer at most once across
/// however many objects it ends up handling, instead of once per object.
#[allow(clippy::too_many_arguments)]
pub fn fetch_object(
    profile: &Profile,
    repo: &Repository,
    rid: RepoId,
    oid: &str,
    size: i64,
    out: &Path,
    signer: &mut Option<MemorySigner>,
) -> anyhow::Result<()> {
    let pointer_text =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n");
    let blob_oid = repo
        .blob(pointer_text.as_bytes())
        .context("failed to write LFS pointer blob")?;

    let candidates = lfs_crypto::find_envelopes(repo, blob_oid)
        .with_context(|| format!("failed to read LFS notes for oid {oid}"))?;
    if candidates.is_empty() {
        anyhow::bail!(
            "no CID recorded for oid {oid}; the peer that has this object needs to push its \
             {} ref",
            lfs_crypto::NOTES_REF
        );
    }

    ipfs::check_daemon()?;

    // Almost always exactly one candidate. More than one only happens if
    // two peers independently encrypted byte-identical content (same
    // oid/size) producing different ciphertext each time -- try each
    // until one actually decrypts for us.
    let mut last_err = None;
    for envelope in candidates {
        let result = match &envelope.enc {
            None => ipfs::cat(&envelope.cid),
            Some(enc) => ipfs::cat(&envelope.cid).and_then(|ciphertext| {
                if signer.is_none() {
                    *signer = Some(lfs_crypto::load_signer(profile)?);
                }
                let signer = signer.as_ref().expect("just populated above");
                lfs_crypto::decrypt_with(&ciphertext, enc, signer, profile.did(), &rid, oid)
            }),
        };
        match result {
            Ok(bytes) => {
                std::fs::write(out, bytes)
                    .with_context(|| format!("failed to write `{}`", out.display()))?;
                return Ok(());
            }
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("no CID recorded for oid {oid}")))
}
