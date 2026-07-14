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

[rad-lfs-transfer]: https://git.hswro.org/mab122/radicle-lfs-transfer

## Prerequisites

- A [Rust toolchain](https://rustup.rs) (stable; see `rust-toolchain.toml` for the exact
  version this workspace pins), Git, and OpenSSH.
- [Git LFS](https://git-lfs.com) (`git-lfs` on your `PATH`) — only needed to *use* `rad lfs`,
  not to build or install anything.
- A local IPFS (Kubo) daemon — also only needed to *use* `rad lfs`, not to build/install.
  Install [Kubo](https://docs.ipfs.tech/install/) and make sure `ipfs` is on your `PATH`.

On Arch Linux:

```sh
sudo pacman -S --needed rust git openssh base-devel   # build
sudo pacman -S --needed git-lfs kubo                  # use `rad lfs`
```

## Build & install

Follows upstream `heartwood`'s own install convention (`cargo install --root ~/.radicle`, one
shared install root for the whole stack), with `radicle-lfs-transfer` added the same way:

```sh
git clone --branch rad-lfs-ipfs --recurse-submodules \
  ssh://git@git.hswro.org:9022/mab122/radicle-heartwood-lfs.git
cd radicle-heartwood-lfs

cargo install --path crates/radicle-cli --force --locked --root ~/.radicle
cargo install --path crates/radicle-node --force --locked --root ~/.radicle
cargo install --path crates/radicle-remote-helper --force --locked --root ~/.radicle
cargo install --path radicle-lfs-transfer --force --locked --root ~/.radicle
```

Add `~/.radicle/bin` to your `PATH` (e.g. in `~/.bashrc`/`~/.zshrc`):

```sh
export PATH="$HOME/.radicle/bin:$PATH"
```

Confirm everything landed correctly:

```sh
rad --version
command -v rad-lfs-transfer   # just needs to resolve; it only speaks git-lfs's stdin/stdout protocol, no --help output
```

If you already cloned without `--recurse-submodules`, run `git submodule update --init` before
building — otherwise `radicle-lfs-transfer/` will be an empty directory and that last
`cargo install` step will fail.

## Run

Nothing extra to run for `rad`/`radicle-node` themselves — use them exactly as upstream
(`rad auth`, `radicle-node`, etc.; see the main [README](README.md) and
[`docs/`](https://app.radicle.xyz/nodes/seed.radicle.xyz/rad:zzz)). The only new moving part is
the IPFS daemon, needed only when you use `rad lfs`:

```sh
ipfs init      # first time only, creates ~/.ipfs
ipfs daemon    # leave running in the background while you use `rad lfs`
```

## Use

Inside a Radicle repository you're a contributor on (i.e. it already has a `rad` remote from
`rad init`/`rad clone`):

```sh
# One-time per repository (with the IPFS daemon above already running)
rad lfs init

# Then it's just Git LFS, as normal
git lfs track "*.psd"          # or whatever large-file patterns you need
git add .gitattributes
git add my-large-file.psd
git commit -m "Add asset"      # pre-commit hook pins it to IPFS and records the CID here
rad push                       # pushes the commit and the refs/notes/rad-lfs mapping together
```

Someone else cloning the same repository:

```sh
rad clone rad:<repo-id>
cd <repo>
rad lfs init                   # one-time, same as above (needs their own IPFS daemon running)
git lfs pull                   # fetches large files via IPFS instead of git
```

`rad seed`/`rad unseed` additionally pin/unpin a repository's known LFS objects in your local
IPFS node, so seeding a repository keeps a full copy of its large files too, not just its git
history.

### What happens without an IPFS daemon running

- `rad lfs init` checks upfront and refuses with a clear message
  (`no IPFS (Kubo) daemon reachable at http://127.0.0.1:5001 — start one with 'ipfs daemon'`)
  rather than silently continuing.
- `rad seed`/`rad unseed` print a warning and skip pinning/unpinning — seeding/unseeding itself
  still succeeds.
- **Nothing else in this fork needs IPFS at all.** Building, installing, and using `rad` for
  anything other than `rad lfs`/pinning works exactly like upstream `heartwood`, with zero IPFS
  dependency.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `` `rad lfs init` must be run inside a Radicle repository working copy `` | Run it inside a directory that already has a `rad` remote (from `rad init` or `rad clone`), not a plain git repo. |
| `Git LFS is not installed (the 'git-lfs' binary was not found on your PATH)` | Install [Git LFS](https://git-lfs.com). |
| `no IPFS (Kubo) daemon reachable at ...` | Start one: `ipfs daemon` (see [Run](#run) above). |
| `rad-lfs-transfer: command not found` during `git lfs push`/`pull` | `cargo install --path radicle-lfs-transfer` didn't complete, or `~/.radicle/bin` isn't on `PATH`. |
| Large file didn't get pinned (commit went through, but no `rad-lfs` note) | The pre-commit hook silently skips pinning if the `ipfs` CLI isn't on `PATH` — check `command -v ipfs`. Re-add the file and commit again once it is. |

## Status

This is a prototype, not upstream Radicle functionality. See the commits on the
`rad-lfs-ipfs` branch for the full change set relative to upstream `heartwood`.
