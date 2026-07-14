# Git LFS support, backed by IPFS

This is a fork of [Radicle's `heartwood`][heartwood] adding Git LFS (large file) support,
with large file content stored on each contributor's own local IPFS node rather than on a
central server.

[heartwood]: https://github.com/radicle-dev/heartwood

## Why not a server?

Radicle's own FAQ notes that its first-generation protocol was built on IPFS and moved away
from it — IPFS is a content-addressed store, and repositories are mutable, so it wasn't a fit
for Radicle's core storage. That objection doesn't apply here: Git LFS's own pattern already
separates a small, mutable pointer file (which Radicle replicates exactly as it replicates any
other git object) from immutable large-file content addressed by hash — which is precisely
what IPFS is good at. This fork only uses IPFS for that second part.

A seed-hosted HTTP LFS server was considered and prototyped first, but rejected: it would have
recreated a single point of failure, and Radicle's node/address book has no existing mechanism
to advertise a seed's HTTP endpoint (only P2P gossip addresses), so real seed discovery would
have needed new protocol work outside this fork's scope. Instead, every peer uses their own
local IPFS node, and the file-CID mapping travels with the repository itself.

## How it works

- **The mapping**: a `cid=<cid>` git note on `refs/notes/rad-lfs`, attached to each LFS
  pointer file's own git blob hash (not the LFS oid — a different value). Verified against
  this repo's actual replication code (`references_of` in
  `crates/radicle/src/storage/git.rs`) that Radicle replicates every ref under a peer's
  namespace except `refs/tmp/heads/*` — there's no fixed allowlist, so this notes ref
  replicates the same way `refs/heads`/`refs/tags`/`refs/cobs` do, once the push/fetch
  refspecs below are configured.
- **On commit**: a pre-commit hook (installed by `rad lfs init`) adds each staged LFS-tracked
  file to your local IPFS daemon, pins it, and writes the note above.
- **On push/pull**: the actual bytes move through
  [`rad-lfs-transfer`][rad-lfs-transfer], a Git LFS custom transfer agent — see that repo for
  details. It's a separate binary/repository since it has nothing Radicle-specific in it; it
  only needs a local IPFS daemon and a git repository with the notes above.
- **Seeding**: `rad seed` now also pins a repository's known LFS objects in your local IPFS
  node (mirroring "seeding = keep a full copy"); `rad unseed` unpins them. Known prototype
  limitation: no cross-repository pin refcounting, so unseeding one repository can unpin
  content still needed by another seeded repository that happens to share the same CID.

[rad-lfs-transfer]: ../radicle-lfs-transfer

## Setup

```sh
# Build & install both binaries (from a checkout with the submodule fetched)
git submodule update --init
cargo install --path .
cargo install --path radicle-lfs-transfer

# One-time, per repository
rad lfs init
```

`rad lfs init` requires a local IPFS (Kubo) daemon to be reachable — it checks upfront and
fails with a clear message (e.g. "start one with `ipfs daemon`") rather than silently
continuing. **Nothing else in this fork requires IPFS.** Building, installing, and using `rad`
for anything other than the `lfs` command works exactly as upstream, with no IPFS dependency
at all. `rad seed`/`rad unseed` will warn (not fail) if a local IPFS daemon isn't reachable
when they try to pin/unpin LFS objects — seeding/unseeding itself still succeeds.

## Status

This is a prototype, not upstream Radicle functionality. See the commits on the
`rad-lfs-ipfs` branch for the full change set relative to upstream `heartwood`.
