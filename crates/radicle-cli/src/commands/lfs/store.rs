//! `rad lfs store` — plumbing command invoked by the Git LFS custom
//! transfer agent (`rad-lfs-transfer`) to upload one LFS object's content
//! to the local IPFS (Kubo) node, encrypting it first if the repository
//! is private.

use std::path::PathBuf;

use anyhow::Context as _;

use radicle::git::raw::Signature;
use radicle::storage::{ReadRepository as _, ReadStorage as _};

use crate::ipfs;
use crate::lfs_crypto::{self, Envelope};
use crate::terminal as term;

/// Committer identity used for the `refs/notes/rad-lfs` notes this command
/// writes. There's no natural "author" for a note that just records a
/// CID, so a fixed identity is used, matching the shell version of this
/// logic in the `rad lfs init`-installed pre-commit hook.
const NOTE_AUTHOR_NAME: &str = "rad-lfs";
const NOTE_AUTHOR_EMAIL: &str = "rad-lfs@localhost";

pub fn run(oid: String, size: i64, path: PathBuf, ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs store` must be run inside a Radicle repository working copy")?;
    let doc = profile.storage.repository(rid)?.identity_doc()?.doc;

    let pointer_text =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n");
    let blob_oid = repo
        .blob(pointer_text.as_bytes())
        .context("failed to write LFS pointer blob")?;

    ipfs::check_daemon()?;

    let envelope = if doc.visibility().is_public() {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        let cid = ipfs::add(&bytes)?;
        let _ = ipfs::pin_all(std::slice::from_ref(&cid));
        Envelope::plain(cid)
    } else {
        let signer = lfs_crypto::load_signer(&profile)?;
        let sender_did = profile.did();
        let recipients = lfs_crypto::recipients_for(&doc, sender_did);
        let plaintext = std::fs::read(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        let (ciphertext, enc) =
            lfs_crypto::encrypt_for(&plaintext, &signer, sender_did, &rid, &oid, &recipients)?;
        let cid = ipfs::add(&ciphertext)?;
        let _ = ipfs::pin_all(std::slice::from_ref(&cid));
        Envelope {
            v: 1,
            cid,
            enc: Some(enc),
        }
    };

    let cid = envelope.cid.clone();
    let message =
        serde_json::to_string(&envelope).context("failed to serialize LFS note envelope")?;
    let signature = Signature::now(NOTE_AUTHOR_NAME, NOTE_AUTHOR_EMAIL)
        .context("failed to construct note signature")?;
    repo.note(
        &signature,
        &signature,
        Some(ipfs::LFS_NOTES_REF),
        blob_oid,
        &message,
        true,
    )
    .context("failed to write LFS note")?;

    term::println(cid);

    Ok(())
}
