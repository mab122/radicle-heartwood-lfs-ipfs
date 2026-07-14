//! Client-side encryption for Git-LFS-over-IPFS objects on private
//! repositories.
//!
//! Radicle's `Visibility::Private` is enforced entirely by access control
//! at the replication layer -- git objects for a private repo are plain,
//! unencrypted objects, protected only by "unauthorized peers are never
//! given them". That protection has no jurisdiction over IPFS's own
//! block-exchange layer: once a legitimate collaborator's IPFS daemon
//! pins LFS content and joins the network, anyone who obtains the CID by
//! any means can fetch the plaintext directly. This module makes sure
//! IPFS only ever stores ciphertext for private repos, using Radicle's
//! own identity keys via envelope encryption (a random per-object content
//! key, wrapped once per authorized recipient).

use std::collections::BTreeSet;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use cyphernet::Ecdh;
use hkdf::Hkdf;
use radicle::Profile;
use radicle::git::raw::{Oid, Repository};
use radicle::identity::{Did, Doc, RepoId, Visibility};
use radicle_crypto::ssh::keystore::MemorySigner;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

pub const ENVELOPE_VERSION: u32 = 1;
const HKDF_DOMAIN: &[u8] = b"radicle-lfs-wrap-v1";
const ALG: &str = "xchacha20poly1305";

/// The local, not-yet-pushed note ref -- `rad lfs store`/`rekey` always
/// write here. Once pushed, this lands in the pusher's own namespace on
/// the server, and `rad lfs init`'s fetch refspec pulls every peer's copy
/// back down under `refs/notes/rad-lfs/<peer>` (see
/// `crates/radicle-remote-helper/src/list.rs`'s `lfs_notes_refs`).
pub const NOTES_REF: &str = "refs/notes/rad-lfs";
/// Glob matching the local ref above plus every fetched peer ref.
const NOTES_REF_GLOB: &str = "refs/notes/rad-lfs*";

/// Note content stored on `refs/notes/rad-lfs`, replacing the earlier
/// bare `cid=<cid>` text. `enc` is `None` for public repos (plain
/// passthrough, unchanged behavior).
#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub cid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enc: Option<Encryption>,
}

impl Envelope {
    #[must_use]
    pub fn plain(cid: String) -> Self {
        Self {
            v: ENVELOPE_VERSION,
            cid,
            enc: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Encryption {
    pub alg: String,
    /// Nonce for the content AEAD. Fixed for the object's life -- `rekey`
    /// never re-encrypts content, only adds wrapped-key entries.
    pub content_nonce: String,
    pub recipients: Vec<WrappedKey>,
}

/// One recipient's wrapped copy of the object's content-encryption key.
/// `sender_did` is per-entry, not per-envelope: a `rekey` operation is
/// performed by a different identity (using their own key) than whoever
/// originally committed the object, so each entry must record who
/// actually produced it.
#[derive(Debug, Serialize, Deserialize)]
pub struct WrappedKey {
    pub did: Did,
    pub sender_did: Did,
    pub salt: String,
    pub wrapped_cek: String,
}

/// Recipients authorized for a repository: delegates, plus the private
/// allow-list (empty for public repos), plus the sender themselves --
/// don't rely on "delegates always includes the author", make it explicit
/// so the committer can always fetch their own commits back.
#[must_use]
pub fn recipients_for(doc: &Doc, sender: Did) -> BTreeSet<Did> {
    let mut set: BTreeSet<Did> = doc.delegates().iter().copied().collect();
    if let Visibility::Private { allow } = doc.visibility() {
        set.extend(allow.iter().copied());
    }
    set.insert(sender);
    set
}

/// Loads a signer capable of ECDH. Only `MemorySigner` implements the
/// `Ecdh` trait -- ssh-agent (the normal day-to-day flow after `rad
/// auth`) can sign but can't expose the raw key material a key-agreement
/// operation needs. This is a hard limitation of the standard ssh-agent
/// protocol, which only implements signing operations, not general key
/// agreement -- there's no ssh-agent request type for it, so this can't
/// be routed through the agent the way signing is.
///
/// The next best thing, matching the exact fallback
/// `crate::terminal::io::signer` already uses elsewhere in this CLI when
/// ssh-agent isn't available or the key isn't registered with it: if
/// connected to a TTY, prompt interactively for the passphrase instead of
/// requiring `RAD_PASSPHRASE` to be pre-set. This covers the common case
/// (an interactive `git commit` triggering the pre-commit hook, which
/// inherits the terminal's TTY) without needing an env var at all; only
/// genuinely non-interactive contexts (CI, a script with no TTY) still
/// need `RAD_PASSPHRASE`, and get a fast, clear failure rather than a
/// hang, since `inquire` (via `radicle_term::io::passphrase`) returns
/// `Ok(None)` on `NotTTY` rather than blocking.
pub fn load_signer(profile: &Profile) -> anyhow::Result<MemorySigner> {
    if !profile.keystore.is_encrypted()? {
        return Ok(MemorySigner::load(&profile.keystore, None)?);
    }
    if let Some(passphrase) = radicle::profile::env::passphrase() {
        return Ok(MemorySigner::load(&profile.keystore, Some(passphrase))?);
    }
    if let Some(passphrase) = prompt_passphrase(profile)? {
        return Ok(MemorySigner::load(&profile.keystore, Some(passphrase))?);
    }
    bail!(
        "private-repo LFS operations need to perform a key-agreement (ECDH) operation, which \
         requires direct access to your secret key -- ssh-agent can sign but can't do this \
         (a protocol limitation, not something to configure around). Run this from a \
         terminal to be prompted for your passphrase, or set RAD_PASSPHRASE, or use an \
         unencrypted keystore."
    )
}

/// Prompts for the keystore passphrase if connected to a TTY. Returns
/// `Ok(None)` (not an error) when there's no TTY to prompt on, so callers
/// can fall through to a clear error instead of hanging.
fn prompt_passphrase(profile: &Profile) -> anyhow::Result<Option<radicle_crypto::ssh::keystore::Passphrase>> {
    let validator = crate::terminal::io::PassphraseValidator::new(profile.keystore.clone());
    Ok(crate::terminal::io::passphrase(validator)?)
}

/// Encrypts `plaintext` with a fresh content key, then wraps that key once
/// per recipient. Returns the ciphertext to store in IPFS and the
/// encryption metadata to record in the note.
pub fn encrypt_for(
    plaintext: &[u8],
    sender: &MemorySigner,
    sender_did: Did,
    rid: &RepoId,
    oid: &str,
    recipients: &BTreeSet<Did>,
) -> anyhow::Result<(Vec<u8>, Encryption)> {
    let cek = XChaCha20Poly1305::generate_key(&mut OsRng);
    let content_nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let cipher = XChaCha20Poly1305::new(&cek);
    let ciphertext = cipher
        .encrypt(&content_nonce, plaintext)
        .map_err(|_| anyhow!("content encryption failed"))?;

    let mut wrapped = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        wrapped.push(wrap_cek_for(&cek, sender, sender_did, recipient, rid, oid)?);
    }

    Ok((
        ciphertext,
        Encryption {
            alg: ALG.to_string(),
            content_nonce: BASE64.encode(content_nonce),
            recipients: wrapped,
        },
    ))
}

pub fn wrap_cek_for(
    cek: &Key,
    sender: &MemorySigner,
    sender_did: Did,
    recipient: &Did,
    rid: &RepoId,
    oid: &str,
) -> anyhow::Result<WrappedKey> {
    let shared = sender
        .ecdh(recipient.as_key())
        .map_err(|e| anyhow!("ECDH with {recipient} failed: {e}"))?;

    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);

    let wrap_key = derive_wrap_key(&shared, &salt, rid, oid, &sender_did, recipient);
    // Fixed all-zero nonce is safe here: `wrap_key` is derived via HKDF
    // from a salt that's freshly generated for this exact
    // (rid, oid, sender, recipient) wrap operation, so it is never reused
    // to encrypt more than this one 32-byte CEK -- key-once nonce reuse
    // is not a vulnerability.
    let cipher = XChaCha20Poly1305::new(&wrap_key);
    let wrapped_cek = cipher
        .encrypt(&XNonce::default(), cek.as_slice())
        .map_err(|_| anyhow!("failed to wrap key for {recipient}"))?;

    Ok(WrappedKey {
        did: *recipient,
        sender_did,
        salt: BASE64.encode(salt),
        wrapped_cek: BASE64.encode(wrapped_cek),
    })
}

/// HKDF context mixes in more than the raw ECDH shared secret: the shared
/// secret between two fixed DIDs is *static*, reused across every object
/// they ever exchange. Without per-object context here, the "nonce fixed
/// because key-once" argument in `wrap_cek_for` would be false -- the same
/// key could end up wrapping multiple different CEKs. Mixing in `rid` +
/// `oid` (plus a fresh random `salt`) makes every wrap operation
/// cryptographically independent, even between the same two DIDs.
fn derive_wrap_key(
    shared: &[u8; 32],
    salt: &[u8],
    rid: &RepoId,
    oid: &str,
    sender_did: &Did,
    recipient_did: &Did,
) -> Key {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut info = Vec::new();
    info.extend_from_slice(HKDF_DOMAIN);
    info.extend_from_slice(rid.to_string().as_bytes());
    info.extend_from_slice(oid.as_bytes());
    info.extend_from_slice(sender_did.to_string().as_bytes());
    info.extend_from_slice(recipient_did.to_string().as_bytes());

    let mut out = [0u8; 32];
    hk.expand(&info, &mut out)
        .expect("32 is a valid HKDF-SHA256 output length");
    out.into()
}

/// Recovers the content-encryption key from the caller's own entry among a
/// set of wrapped keys, without needing the ciphertext at all -- this is
/// the exact primitive `rad lfs rekey` needs (it only adds new wrapped-key
/// entries for the same never-changing CEK, it never re-encrypts content).
pub fn unwrap_cek(
    recipients: &[WrappedKey],
    me: &MemorySigner,
    my_did: Did,
    rid: &RepoId,
    oid: &str,
) -> anyhow::Result<Zeroizing<[u8; 32]>> {
    let entry = recipients
        .iter()
        .find(|w| w.did == my_did)
        .ok_or_else(|| anyhow!("you are not an authorized recipient of this object"))?;

    let shared = me
        .ecdh(entry.sender_did.as_key())
        .map_err(|e| anyhow!("ECDH with {} failed: {e}", entry.sender_did))?;
    let salt = BASE64
        .decode(&entry.salt)
        .context("invalid salt in wrapped key entry")?;
    let wrap_key = derive_wrap_key(&shared, &salt, rid, oid, &entry.sender_did, &my_did);
    let wrapped_cek = BASE64
        .decode(&entry.wrapped_cek)
        .context("invalid wrapped content key")?;

    let cipher = XChaCha20Poly1305::new(&wrap_key);
    let cek = cipher
        .decrypt(&XNonce::default(), wrapped_cek.as_slice())
        .map_err(|_| anyhow!("failed to unwrap key -- wrong key or corrupted data"))?;

    let mut out = [0u8; 32];
    out.copy_from_slice(&cek);
    Ok(Zeroizing::new(out))
}

/// Decrypts object content using the caller's own wrapped-key entry.
pub fn decrypt_with(
    ciphertext: &[u8],
    enc: &Encryption,
    me: &MemorySigner,
    my_did: Did,
    rid: &RepoId,
    oid: &str,
) -> anyhow::Result<Vec<u8>> {
    let cek = unwrap_cek(&enc.recipients, me, my_did, rid, oid)?;
    let nonce_bytes = BASE64
        .decode(&enc.content_nonce)
        .context("invalid content nonce")?;
    let nonce = XNonce::from_slice(&nonce_bytes);
    let key: Key = (*cek).into();
    let cipher = XChaCha20Poly1305::new(&key);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("content decryption failed -- wrong key or corrupted data"))
}

/// Finds every distinct envelope recorded for `blob_oid`, across the
/// local `refs/notes/rad-lfs` ref and every fetched peer ref under
/// `refs/notes/rad-lfs/<peer>`. Notes are per-peer contributions -- any
/// peer can commit an LFS-tracked file, not just delegates -- so more
/// than one peer's note can exist for the same object, e.g. after a
/// `rad lfs rekey` that added recipients under a different peer's
/// namespace than the original committer.
///
/// Notes sharing the same `cid` are the *same* encrypted object (rekey
/// never changes the ciphertext, only adds wrapped-key entries for it),
/// so their recipient lists are safely unioned together. Notes with
/// *different* `cid`s (which can only happen if two peers independently
/// encrypted byte-identical plaintext -- same oid/size, since that's
/// what determines the pointer blob -- producing different ciphertext
/// each time since encryption uses a fresh random key/nonce) are kept as
/// separate candidates, since a wrapped-key entry from one is only valid
/// against its own ciphertext, never the other's. Callers that need to
/// decrypt should try each returned candidate in turn.
pub fn find_envelopes(repo: &Repository, blob_oid: Oid) -> anyhow::Result<Vec<Envelope>> {
    let mut by_cid: Vec<Envelope> = Vec::new();

    for name in notes_ref_names(repo)? {
        let Ok(note) = repo.find_note(Some(&name), blob_oid) else {
            continue;
        };
        let Some(message) = note.message() else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(message) else {
            continue;
        };

        match by_cid.iter_mut().find(|e| e.cid == envelope.cid) {
            Some(existing) => merge_recipients(existing, envelope),
            None => by_cid.push(envelope),
        }
    }

    Ok(by_cid)
}

/// Convenience wrapper for callers that only need the set of distinct
/// CIDs recorded for `blob_oid` (e.g. seed/unseed pinning, which needs
/// no key material and doesn't care about recipients at all).
pub fn find_cids(repo: &Repository, blob_oid: Oid) -> anyhow::Result<Vec<String>> {
    Ok(find_envelopes(repo, blob_oid)?
        .into_iter()
        .map(|e| e.cid)
        .collect())
}

fn merge_recipients(into: &mut Envelope, other: Envelope) {
    let (Some(into_enc), Some(other_enc)) = (into.enc.as_mut(), other.enc) else {
        return;
    };
    let existing: BTreeSet<Did> = into_enc.recipients.iter().map(|w| w.did).collect();
    into_enc
        .recipients
        .extend(other_enc.recipients.into_iter().filter(|w| !existing.contains(&w.did)));
}

/// Every distinct blob oid that has at least one note recorded against it,
/// across every notes ref (local plus every fetched peer). Used by
/// `rad lfs rekey` to walk every known LFS object, since a single
/// `notes()` call on one ref would now miss objects whose only note lives
/// under a different peer's ref.
pub fn all_note_targets(repo: &Repository) -> anyhow::Result<BTreeSet<Oid>> {
    let mut targets = BTreeSet::new();
    for name in notes_ref_names(repo)? {
        let Ok(notes) = repo.notes(Some(&name)) else {
            continue;
        };
        for (_, target_oid) in notes.flatten() {
            targets.insert(target_oid);
        }
    }
    Ok(targets)
}

fn notes_ref_names(repo: &Repository) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for reference in repo.references_glob(NOTES_REF_GLOB)? {
        let reference = reference?;
        if let Some(name) = reference.name() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}
