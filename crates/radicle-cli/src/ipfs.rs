//! Pinning of Git-LFS-backed content in a local IPFS (Kubo) node.
//!
//! This is a prototype of "Git-LFS over IPFS": large files tracked via
//! Git LFS are stored in IPFS rather than as git blobs, and the mapping
//! from an LFS pointer file's git blob oid to the corresponding IPFS CID
//! is recorded as a git note on `refs/notes/rad-lfs` (note content is a
//! JSON [`crate::lfs_crypto::Envelope`], e.g. `{"v":1,"cid":"Qm...",
//! "enc":null}` for a public repo, or with populated `enc` metadata for
//! a private one).
//!
//! Since seeding a repository already means "keep a full local copy of
//! it", we extend that lifecycle to IPFS: seeding a repository pins all
//! of its known LFS objects locally, and unseeding unpins them.

use std::env;

use anyhow::anyhow;
use radicle::git::raw::Repository;

use crate::terminal as term;

/// Git notes ref under which oid -> CID mappings for LFS objects are stored.
///
/// This is the *local* ref name; readers need to merge across every
/// fetched peer's copy too (`refs/notes/rad-lfs/<peer>`) -- see
/// `crate::lfs_crypto::find_envelopes`/`all_note_targets`, which
/// `lfs_cids` below delegates to.
pub use crate::lfs_crypto::NOTES_REF as LFS_NOTES_REF;

/// Default HTTP API address of a local Kubo (IPFS) daemon.
const DEFAULT_KUBO_API_URL: &str = "http://127.0.0.1:5001";

/// Cap on how large an LFS object's content we'll read back from IPFS in
/// one go. `ureq`'s `Body::read_to_vec()` otherwise defaults to a 10MB
/// limit meant for typical HTTP API responses, which is far too small for
/// LFS content (that's the whole point of storing it out-of-band). 1GiB
/// comfortably covers any file worth tracking in this repo; raise if a
/// larger LFS object legitimately needs to be fetched.
const MAX_CAT_RESPONSE_BYTES: u64 = 1024 * 1024 * 1024;

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

/// Collect the set of CIDs referenced by LFS notes, across every peer's
/// notes ref (not just our own — a repo we're seeding may hold objects
/// whose only note came from someone else's `refs/notes/rad-lfs/<peer>`).
///
/// Returns an empty vector if no notes exist, i.e. the repository has no
/// LFS objects tracked yet. This is not an error.
#[must_use]
pub fn lfs_cids(repo: &Repository) -> Vec<String> {
    let Ok(targets) = crate::lfs_crypto::all_note_targets(repo) else {
        return Vec::new();
    };

    let mut cids = Vec::new();
    for target in targets {
        if let Ok(found) = crate::lfs_crypto::find_cids(repo, target) {
            cids.extend(found);
        }
    }
    cids
}

/// Add `bytes` to the local IPFS node as a single blob, returning its CID.
pub fn add(bytes: &[u8]) -> anyhow::Result<String> {
    use ureq::unversioned::multipart::{Form, Part};

    let base = kubo_api_url();
    let url = format!("{base}/api/v0/add");

    let form = Form::new().part("file", Part::bytes(bytes).file_name("blob"));

    let mut response = ureq::post(&url).send(form).map_err(|err| {
        if is_daemon_unreachable(&err) {
            anyhow!(
                "no IPFS (Kubo) daemon reachable at {base} — start one with `ipfs daemon`.\n\
                 This repo's large-file (LFS) support is backed by your local IPFS node."
            )
        } else {
            anyhow!("failed to add object to IPFS: {err}")
        }
    })?;

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| anyhow!("failed to read IPFS `add` response: {err}"))?;
    // The response is newline-delimited JSON, one object per added file;
    // we only ever add a single blob per call, so the first line is all
    // we need.
    let line = body
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty response from IPFS `add`"))?;
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|err| anyhow!("failed to parse IPFS `add` response: {err}"))?;
    let hash = value
        .get("Hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow!("IPFS `add` response missing a `Hash` field"))?;

    Ok(hash.to_string())
}

/// Fetch the raw bytes of the object identified by `cid` from the local
/// IPFS node.
pub fn cat(cid: &str) -> anyhow::Result<Vec<u8>> {
    let base = kubo_api_url();
    let url = format!("{base}/api/v0/cat?arg={cid}");

    let mut response = ureq::post(&url).send_empty().map_err(|err| {
        if is_daemon_unreachable(&err) {
            anyhow!(
                "no IPFS (Kubo) daemon reachable at {base} — start one with `ipfs daemon`.\n\
                 This repo's large-file (LFS) support is backed by your local IPFS node."
            )
        } else {
            anyhow!("failed to fetch object {cid} from IPFS: {err}")
        }
    })?;

    response
        .body_mut()
        .with_config()
        .limit(MAX_CAT_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(|err| anyhow!("failed to read IPFS `cat` response for {cid}: {err}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the `cat()` response being truncated at
    /// `ureq`'s default 10MB read limit for any LFS object larger than
    /// that (e.g. the ~29MB `assets/posts/hashcash/thumbnail.xcf` in the
    /// blog repo failed to smudge with exactly this error before the
    /// `MAX_CAT_RESPONSE_BYTES` fix). Requires a local Kubo daemon, so
    /// it's `#[ignore]`d by default: `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires a local IPFS (Kubo) daemon at 127.0.0.1:5001"]
    fn cat_round_trips_content_larger_than_ureqs_default_10mb_limit() {
        check_daemon().expect("local Kubo daemon must be reachable for this test");

        let bytes = vec![0x42u8; 15 * 1024 * 1024]; // 15MB, past the old 10MB cap
        let cid = add(&bytes).expect("add should succeed");
        let fetched = cat(&cid).expect("cat should succeed for content past the old 10MB cap");

        assert_eq!(fetched.len(), bytes.len());
        assert_eq!(fetched, bytes);

        unpin_all(std::slice::from_ref(&cid));
    }
}
