pub mod args;

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
    // Git-LFS objects.
    if !cids.is_empty() {
        crate::ipfs::unpin_all(&cids);
    }

    Ok(())
}
