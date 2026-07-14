//! `rad lfs fetch` — plumbing command invoked by the Git LFS custom
//! transfer agent (`rad-lfs-transfer`) to download one LFS object's
//! content from the local IPFS (Kubo) node, decrypting it first if the
//! repository is private.

use std::path::PathBuf;

use anyhow::{Context as _, anyhow};

use crate::ipfs;
use crate::lfs_crypto::{self, Envelope};
use crate::terminal as term;

pub fn run(
    oid: String,
    size: i64,
    out: PathBuf,
    ctx: impl term::Context,
) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs fetch` must be run inside a Radicle repository working copy")?;

    let pointer_text =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n");
    let blob_oid = repo
        .blob(pointer_text.as_bytes())
        .context("failed to write LFS pointer blob")?;

    let note = repo.find_note(Some(ipfs::LFS_NOTES_REF), blob_oid).map_err(|_| {
        anyhow!(
            "no CID recorded for oid {oid}; the peer that has this object needs to push its \
             {} ref",
            ipfs::LFS_NOTES_REF
        )
    })?;
    let message = note.message().ok_or_else(|| {
        anyhow!(
            "no CID recorded for oid {oid}; the peer that has this object needs to push its \
             {} ref",
            ipfs::LFS_NOTES_REF
        )
    })?;
    let envelope: Envelope = serde_json::from_str(message)
        .with_context(|| format!("failed to parse LFS note for oid {oid}"))?;

    ipfs::check_daemon()?;

    let bytes = match &envelope.enc {
        None => ipfs::cat(&envelope.cid)?,
        Some(enc) => {
            let ciphertext = ipfs::cat(&envelope.cid)?;
            let signer = lfs_crypto::load_signer(&profile)?;
            lfs_crypto::decrypt_with(&ciphertext, enc, &signer, profile.did(), &rid, &oid)?
        }
    };

    std::fs::write(&out, bytes)
        .with_context(|| format!("failed to write `{}`", out.display()))?;

    Ok(())
}
