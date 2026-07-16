//! `rad lfs store` — plumbing command invoked by the Git LFS custom
//! transfer agent (`rad-lfs-transfer`) to upload one LFS object's content
//! to the local IPFS (Kubo) node, encrypting it first if the repository
//! is private.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context as _;

use radicle::Profile;
use radicle::git::raw::{Repository, Signature};
use radicle::identity::{Doc, RepoId};
use radicle::storage::{ReadRepository as _, ReadStorage as _};
use radicle_crypto::ssh::keystore::MemorySigner;

use crate::ipfs;
use crate::lfs_crypto::{self, Envelope, LOCAL_NOTES_REF, NOTE_AUTHOR_EMAIL, NOTE_AUTHOR_NAME};
use crate::terminal as term;

pub fn run(oid: String, size: i64, path: PathBuf, ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs store` must be run inside a Radicle repository working copy")?;
    let doc = profile.storage.repository(rid)?.identity_doc()?.doc;

    let mut signer = None;
    let cid = store_object(&profile, &repo, rid, &doc, &oid, size, &path, &mut signer)?;
    term::println(cid);

    Ok(())
}

/// Stores one LFS object's content in IPFS (encrypting first for private
/// repos) and records the resulting CID as a git note. Factored out of
/// [`run`] so `rad lfs precommit` can process every staged LFS file in a
/// single process, loading (and prompting for) the signer at most once
/// across the whole batch instead of once per file.
#[allow(clippy::too_many_arguments)]
pub fn store_object(
    profile: &Profile,
    repo: &Repository,
    rid: RepoId,
    doc: &Doc,
    oid: &str,
    size: i64,
    path: &Path,
    signer: &mut Option<MemorySigner>,
) -> anyhow::Result<String> {
    let pointer_text =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n");
    let blob_oid = repo
        .blob(pointer_text.as_bytes())
        .context("failed to write LFS pointer blob")?;

    // Fast path: a note already recorded for this object (typically written
    // moments ago by `rad lfs precommit`, or fetched from a peer) means the
    // content is already pinned in IPFS -- nothing left to do. This matters
    // beyond just avoiding duplicate work: `git push`'s custom-transfer-agent
    // subprocess chain has no TTY (its stdio is consumed by the transfer
    // protocol, not a terminal), so without this fast path, a private repo's
    // `git push` would call back into `store_object` per file and fail
    // outright on the passphrase prompt every time, even though the actual
    // encryption work was already done (and the passphrase already
    // supplied) once, interactively, at commit time.
    if let Some(cid) = lfs_crypto::find_cids(repo, blob_oid)?.into_iter().next() {
        return Ok(cid);
    }

    ipfs::check_daemon()?;

    let envelope = if doc.visibility().is_public() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        let cid = ipfs::add(&bytes)?;
        let _ = ipfs::pin_all(std::slice::from_ref(&cid));
        Envelope::plain(cid)
    } else {
        if signer.is_none() {
            *signer = Some(lfs_crypto::load_signer(profile)?);
        }
        let signer = signer.as_ref().expect("just populated above");
        let sender_did = profile.did();
        let recipients = lfs_crypto::recipients_for(doc, sender_did);
        let plaintext = std::fs::read(path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        let (ciphertext, enc) =
            lfs_crypto::encrypt_for(&plaintext, signer, sender_did, &rid, oid, &recipients)?;
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
    repo.note(&signature, &signature, Some(LOCAL_NOTES_REF), blob_oid, &message, true)
        .context("failed to write LFS note")?;

    Ok(cid)
}
