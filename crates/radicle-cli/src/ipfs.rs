//! Pinning of Git-LFS-backed content in a local IPFS (Kubo) node.
//!
//! This is a prototype of "Git-LFS over IPFS": large files tracked via
//! Git LFS are stored in IPFS rather than as git blobs, and the mapping
//! from an LFS pointer file's git blob oid to the corresponding IPFS CID
//! is recorded as a git note on `refs/notes/rad-lfs` (note content
//! `cid=<cid>`).
//!
//! Since seeding a repository already means "keep a full local copy of
//! it", we extend that lifecycle to IPFS: seeding a repository pins all
//! of its known LFS objects locally, and unseeding unpins them.

use std::env;

use anyhow::anyhow;
use radicle::git::raw::Repository;

use crate::terminal as term;

/// Git notes ref under which oid -> CID mappings for LFS objects are stored.
pub const LFS_NOTES_REF: &str = "refs/notes/rad-lfs";

/// Default HTTP API address of a local Kubo (IPFS) daemon.
const DEFAULT_KUBO_API_URL: &str = "http://127.0.0.1:5001";

/// Returns the Kubo HTTP API base URL, respecting the `KUBO_API_URL`
/// environment variable if set.
pub(crate) fn kubo_api_url() -> String {
    env::var("KUBO_API_URL").unwrap_or_else(|_| DEFAULT_KUBO_API_URL.to_string())
}

/// Check that a local IPFS (Kubo) daemon is reachable, failing with a
/// clear, actionable error otherwise. Used by `rad lfs init` to refuse to
/// proceed silently when there's no daemon to back LFS content with.
pub fn check_daemon() -> anyhow::Result<()> {
    let base = kubo_api_url();
    let url = format!("{base}/api/v0/id");

    match ureq::post(&url).send_empty() {
        Ok(_) => Ok(()),
        Err(err) if is_daemon_unreachable(&err) => Err(anyhow!(
            "no IPFS (Kubo) daemon reachable at {base} — start one with `ipfs daemon`.\n\
             This repo's large-file (LFS) support is backed by your local IPFS node."
        )),
        Err(err) => Err(anyhow!("IPFS daemon at {base} responded unexpectedly: {err}")),
    }
}

/// Collect the set of CIDs referenced by LFS notes on [`LFS_NOTES_REF`].
///
/// Returns an empty vector if the notes ref doesn't exist, i.e. the
/// repository has no LFS objects tracked yet. This is not an error.
#[must_use]
pub fn lfs_cids(repo: &Repository) -> Vec<String> {
    let notes = match repo.notes(Some(LFS_NOTES_REF)) {
        Ok(notes) => notes,
        // The `refs/notes/rad-lfs` ref doesn't exist, which just means
        // there are no LFS objects tracked for this repository yet.
        Err(_) => return Vec::new(),
    };

    let mut cids = Vec::new();
    for note in notes {
        let Ok((_, annotated_id)) = note else {
            continue;
        };
        let Ok(note) = repo.find_note(Some(LFS_NOTES_REF), annotated_id) else {
            continue;
        };
        let Some(message) = note.message() else {
            continue;
        };
        if let Some(cid) = message.trim().strip_prefix("cid=") {
            cids.push(cid.to_string());
        }
    }
    cids
}

/// Whether a `ureq` error indicates that the local IPFS daemon could
/// not be reached at all, as opposed to an HTTP-level error response.
fn is_daemon_unreachable(err: &ureq::Error) -> bool {
    matches!(
        err,
        ureq::Error::ConnectionFailed
            | ureq::Error::Io(_)
            | ureq::Error::HostNotFound
            | ureq::Error::Timeout(_)
    )
}

/// Pin the given CIDs recursively in the local IPFS daemon.
///
/// Returns the number of CIDs successfully pinned. If the local daemon
/// is not reachable, a warning is printed and `0` is returned; this is
/// not treated as a hard failure, since seeding itself already succeeded.
#[must_use]
pub fn pin_all(cids: &[String]) -> usize {
    let base = kubo_api_url();
    let mut pinned = 0;

    for cid in cids {
        let url = format!("{base}/api/v0/pin/add?arg={cid}&recursive=true");
        match ureq::post(&url).send_empty() {
            Ok(_) => pinned += 1,
            Err(err) if is_daemon_unreachable(&err) => {
                term::warning(format!(
                    "Could not reach local IPFS daemon at {base}; LFS objects were not pinned"
                ));
                return pinned;
            }
            Err(err) => {
                term::warning(format!("Failed to pin LFS object {cid} in IPFS: {err}"));
            }
        }
    }

    pinned
}

/// Unpin the given CIDs from the local IPFS daemon.
///
/// If the local daemon is not reachable, a warning is printed and the
/// function returns without failing; this is not treated as a hard
/// failure, since unseeding itself already succeeded.
pub fn unpin_all(cids: &[String]) {
    let base = kubo_api_url();

    for cid in cids {
        let url = format!("{base}/api/v0/pin/rm?arg={cid}");
        // NOTE: this unconditionally unpins `cid`, even if it's still
        // referenced by LFS objects of another repository that remains
        // seeded. This prototype has no cross-repo pin refcounting, so
        // a CID shared between repositories will be unpinned here
        // regardless of whether other seeded repos still need it.
        match ureq::post(&url).send_empty() {
            Ok(_) => {}
            Err(err) if is_daemon_unreachable(&err) => {
                term::warning(format!(
                    "Could not reach local IPFS daemon at {base}; LFS objects were not unpinned"
                ));
                return;
            }
            Err(err) => {
                term::warning(format!("Failed to unpin LFS object {cid} in IPFS: {err}"));
            }
        }
    }
}
