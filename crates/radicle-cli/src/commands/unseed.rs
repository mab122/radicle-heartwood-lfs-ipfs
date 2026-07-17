pub mod args;

use std::collections::BTreeSet;

use radicle::node::policy;
use radicle::{Node, prelude::*};

use crate::terminal as term;

pub use args::Args;

pub fn run(args: Args, ctx: impl term::Context) -> anyhow::Result<()> {
    let profile = ctx.profile()?;
    let mut node = radicle::Node::new(profile.socket_from_env());

    for rid in args.rids {
        delete(rid, &mut node, &profile)?;
    }

    Ok(())
}

pub fn delete(rid: RepoId, node: &mut Node, profile: &Profile) -> anyhow::Result<()> {
    // Gather the repository's known LFS objects *before* unseeding, since
    // we still need access to its git notes at that point.
    let cids = profile
        .storage
        .repository(rid)
        .map(|repo| crate::ipfs::lfs_cids(&repo.backend))
        .unwrap_or_default();

    if profile.unseed(rid, node)? {
        term::success!("Seeding policy for {} removed", term::format::tertiary(rid));
    }

    // Mirror the seed/unseed lifecycle for IPFS pins of this repository's
    // Git-LFS objects -- except any CID also referenced by an LFS note in
    // some *other* still-seeded repository (e.g. two repositories that
    // happen to both track a byte-identical file, giving it the same
    // content-addressed CID). Unpinning unconditionally would evict
    // content the other repository still needs, breaking it silently the
    // next time someone there runs `git lfs fetch`. There's no
    // cross-repository pin refcount kept anywhere -- this recomputes the
    // still-needed set from every other seeded repository's own notes each
    // time, which costs a pass over every other seeded repository but
    // requires no extra persistent state, consistent with this design's
    // git-notes-as-source-of-truth approach everywhere else.
    if !cids.is_empty() {
        let still_needed = cids_still_needed_elsewhere(profile, rid)?;
        let skipped = cids.iter().filter(|cid| still_needed.contains(*cid)).count();
        let safe_to_unpin: Vec<String> =
            cids.into_iter().filter(|cid| !still_needed.contains(cid)).collect();

        if !safe_to_unpin.is_empty() {
            crate::ipfs::unpin_all(&safe_to_unpin);
        }
        if skipped > 0 {
            term::info!(
                "Kept {skipped} IPFS object(s) pinned -- still referenced by another seeded repository"
            );
        }
    }

    Ok(())
}

/// The set of CIDs (among `excluding`'s own) that are also referenced by an
/// LFS note in some other repository this node is currently seeding.
fn cids_still_needed_elsewhere(
    profile: &Profile,
    excluding: RepoId,
) -> anyhow::Result<BTreeSet<String>> {
    let mut needed = BTreeSet::new();
    let store = profile.policies()?;

    for entry in store.seed_policies()? {
        let policy::SeedPolicy { rid, policy } = entry?;
        if rid == excluding {
            continue;
        }
        if !matches!(policy, policy::SeedingPolicy::Allow { .. }) {
            continue;
        }
        if let Ok(repo) = profile.storage.repository(rid) {
            needed.extend(crate::ipfs::lfs_cids(&repo.backend));
        }
    }

    Ok(needed)
}
