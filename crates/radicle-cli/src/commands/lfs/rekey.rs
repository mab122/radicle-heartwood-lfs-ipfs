//! `rad lfs rekey` — grants newly-authorized collaborators (new delegates,
//! or newly-added entries in a private repo's allow-list) access to
//! previously-encrypted LFS objects, without re-encrypting or re-uploading
//! any content: it only adds new wrapped-key entries to each object's
//! note for recipients who are missing one.

use std::collections::BTreeSet;

use anyhow::Context as _;

use radicle::git::raw::Signature;
use radicle::identity::Did;
use radicle::storage::{ReadRepository as _, ReadStorage as _};

use crate::lfs_crypto::{self, LOCAL_NOTES_REF, NOTE_AUTHOR_EMAIL, NOTE_AUTHOR_NAME};
use crate::terminal as term;

pub fn run(ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let (repo, rid) = radicle::rad::cwd()
        .context("`rad lfs rekey` must be run inside a Radicle repository working copy")?;
    let doc = profile.storage.repository(rid)?.identity_doc()?.doc;

    let signer = lfs_crypto::load_signer(&profile)?;
    let my_did = profile.did();

    // The full set of currently-authorized DIDs: delegates ∪ the private
    // allow-list ∪ `my_did` itself (the `sender` parameter of
    // `recipients_for` is only meaningful for `encrypt_for`'s "make sure
    // the committer can read their own object back" guarantee; here we
    // just want the full authorized set, and `my_did` is necessarily
    // already a member of it since running `rekey` requires being
    // authorized).
    let current = lfs_crypto::recipients_for(&doc, my_did);

    // Walk every note across every peer's notes ref -- not just the local
    // one -- since an object's only note may live under a peer we've
    // fetched from rather than one we wrote ourselves.
    let targets = lfs_crypto::all_note_targets(&repo)
        .context("failed to enumerate LFS notes")?;
    if targets.is_empty() {
        term::success!("Nothing to rekey: no LFS objects are tracked in this repository yet");
        return Ok(());
    }

    let signature = Signature::now(NOTE_AUTHOR_NAME, NOTE_AUTHOR_EMAIL)
        .context("failed to construct note signature")?;

    let mut rekeyed_objects = 0;
    let mut added_recipients = 0;

    for target_oid in targets {
        // The note only stores the CID and encryption metadata, not the
        // oid/size that were used as HKDF context when wrapping the
        // content key. Recover them losslessly from the pointer blob
        // itself: `target_oid` *is* the git blob hash of the exact
        // deterministic pointer text `store`/`fetch` compute from
        // oid/size, so we can parse it back out.
        let Ok(blob) = repo.find_blob(target_oid) else {
            continue;
        };
        let Ok(pointer_text) = std::str::from_utf8(blob.content()) else {
            continue;
        };
        let Some(oid) = pointer_text
            .lines()
            .find_map(|line| line.strip_prefix("oid sha256:"))
        else {
            continue;
        };

        let candidates = match lfs_crypto::find_envelopes(&repo, target_oid) {
            Ok(candidates) => candidates,
            Err(err) => {
                term::warning(format!("Skipping object with oid {oid}: {err}"));
                continue;
            }
        };

        // Almost always exactly one candidate; more than one only if two
        // peers independently encrypted byte-identical content (same
        // oid/size) producing different ciphertext each time. Rekey
        // whichever ones we're actually authorized to unwrap -- skip the
        // rest quietly, they're not our concern.
        for mut envelope in candidates {
            let Some(enc) = envelope.enc.as_mut() else {
                // Public (unencrypted) object: nothing to rekey.
                continue;
            };

            let cek = match lfs_crypto::unwrap_cek(&enc.recipients, &signer, my_did, &rid, oid) {
                Ok(cek) => cek,
                Err(_) => {
                    // Not authorized for *this particular* candidate --
                    // expected and not worth warning about, since another
                    // candidate for the same object (or none) may apply.
                    continue;
                }
            };

            let existing: BTreeSet<Did> = enc.recipients.iter().map(|w| w.did).collect();
            let missing: Vec<Did> = current.difference(&existing).copied().collect();
            if missing.is_empty() {
                continue;
            }

            let cek_key: chacha20poly1305::Key = (*cek).into();
            let mut any_added = false;
            for recipient in &missing {
                match lfs_crypto::wrap_cek_for(&cek_key, &signer, my_did, recipient, &rid, oid) {
                    Ok(wrapped) => {
                        enc.recipients.push(wrapped);
                        added_recipients += 1;
                        any_added = true;
                    }
                    Err(err) => {
                        term::warning(format!(
                            "Failed to wrap the content key for {recipient} on object with oid \
                             {oid}: {err}"
                        ));
                    }
                }
            }

            if !any_added {
                continue;
            }

            // Always write to our own local ref, regardless of which
            // peer's ref the original note came from -- once pushed, this
            // lands under our own namespace and is picked up by others
            // via the fetch refspec, alongside (not replacing) the
            // original note.
            let message = serde_json::to_string(&envelope)
                .context("failed to serialize LFS note envelope")?;
            repo.note(&signature, &signature, Some(LOCAL_NOTES_REF), target_oid, &message, true)
                .context("failed to write LFS note")?;

            rekeyed_objects += 1;
        }
    }

    if rekeyed_objects == 0 {
        term::success!("Nothing to rekey: all recorded recipients already have access");
    } else {
        term::success!(
            "Rekeyed {rekeyed_objects} object(s), added {added_recipients} new recipient(s)"
        );
    }

    Ok(())
}
