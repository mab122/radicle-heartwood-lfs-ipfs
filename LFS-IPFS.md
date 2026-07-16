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

There's no server anywhere in this design: every peer uses their own local IPFS node, and the
file-CID mapping travels with the repository itself, avoiding a single point of failure.

## How it works

- **The mapping**: a JSON git note, attached to each LFS pointer file's own git blob hash (not
  the LFS oid — a different value), e.g. `{"v":1,"cid":"Qm..."}` for a public-repo object.
  Written locally to `refs/notes/rad-lfs/local` rather than the bare `refs/notes/rad-lfs`
  (writing to the bare ref directly causes a git ref D/F conflict once a peer's note has been
  fetched — see the `rad lfs init` push/fetch refspecs below); `rad lfs init`'s push refspec
  maps that local ref to the bare `refs/notes/rad-lfs` name on the remote, which is what other
  peers then fetch as `refs/notes/rad-lfs/<your-peer-id>`. Verified against this repo's actual
  replication code (`references_of` in `crates/radicle/src/storage/git.rs`) that Radicle
  replicates every ref under a peer's namespace except `refs/tmp/heads/*` — there's no fixed
  allowlist, so these notes refs replicate the same way `refs/heads`/`refs/tags`/`refs/cobs` do,
  once the push/fetch refspecs below are configured.
- **On commit**: a pre-commit hook (installed by `rad lfs init`) computes every staged
  LFS-tracked file's oid/size and passes them all to a single `rad lfs precommit` call, which
  adds each one to your local IPFS daemon, pins it, and writes the note above — encrypting the
  content first if the repository is private (see below). Batched into one call (rather than one
  `rad lfs store` call per file) specifically so a private repo's passphrase is only prompted
  for once per commit, not once per file.
- **On push**: `rad lfs init` configures `remote.rad.push` with *two* refspecs -- one for your
  branches (`+refs/heads/*:refs/heads/*`), one for the notes mapping
  (`+refs/notes/rad-lfs/local:refs/notes/rad-lfs`) -- so a single bare `git push rad` pushes
  your commit and its notes mapping together. This wasn't always true: earlier builds only configured the notes
  refspec, so a bare `git push rad` silently pushed *only* the mapping, leaving the commit
  itself unpushed -- the single most common way to end up with content that looks committed but
  that a collaborator can't fetch (`no CID recorded for oid ...`). Fixed, but only takes effect
  once `rad lfs init` has been (re-)run with a build that includes the fix -- see
  [Troubleshooting](#troubleshooting) below if you're still seeing the old behavior.
- **The actual bytes** move through [`rad-lfs-transfer`][rad-lfs-transfer], a Git LFS custom
  transfer agent invoked automatically by `git`/`git-lfs` during a push or checkout — see that
  repo for details. It's a separate binary/repository since it has nothing Radicle-specific in
  it (no identity, no keys, no encryption); it shells out to `rad lfs store`/`rad lfs fetch` for
  the actual work, which is a no-op re-upload if the object already has a note (e.g. one your
  own pre-commit hook already wrote).
- **Seeding**: `rad seed` now also pins a repository's known LFS objects in your local IPFS
  node (mirroring "seeding = keep a full copy"); `rad unseed` unpins them. Known prototype
  limitation: no cross-repository pin refcounting, so unseeding one repository can unpin
  content still needed by another seeded repository that happens to share the same CID.

## Encryption for private repositories

Radicle's `Visibility::Private` is enforced entirely by access control at the replication
layer — git objects for a private repo are plain, unencrypted objects, protected only by
"unauthorized peers are never given them" (see `crates/radicle/src/identity/doc.rs`). That
protection has no jurisdiction over IPFS's own block-exchange layer: once a legitimate
collaborator's IPFS daemon pins content and joins the network, anyone who obtains the CID by
any means can fetch the plaintext directly. So for private repositories, `rad lfs store`
encrypts content before it ever reaches IPFS:

- A random per-object content key (CEK) encrypts the file (XChaCha20-Poly1305).
- The CEK is wrapped once per authorized recipient (repo delegates + the private allow-list),
  using ECDH between the committer's and each recipient's Radicle identity key (Ed25519,
  directly on the curve — no separate encryption keypair to manage) plus HKDF, so only
  identities Radicle already considers authorized for the repo can decrypt.
- The wrapped keys travel in the same git note as the CID — no separate key-distribution
  mechanism, no server.

**This requires a signer capable of key agreement (ECDH), not just signing.** ssh-agent — the
normal day-to-day flow after `rad auth` — can sign but can't expose the key material ECDH
needs; that's a hard limitation of the standard ssh-agent protocol (it only implements
signing, there's no request type for general key agreement), not something to configure
around. `rad lfs store`/`rad lfs fetch`/`rad lfs rekey` on a *private* repository fall back to
whichever of these applies, in order:

1. An unencrypted keystore, if that's how you've set things up.
2. `RAD_PASSPHRASE`, if set in the environment.
3. **An interactive passphrase prompt**, if connected to a terminal — the same fallback
   `rad`'s other commands already use when ssh-agent isn't available or the key isn't
   registered with it. This covers the common case (an interactive `git commit` triggering
   the pre-commit hook, which inherits the terminal) with no env var needed at all.

Only a genuinely non-interactive context (CI, a script with no TTY, `RAD_PASSPHRASE` unset)
hits a hard failure — and it fails fast with a clear message, rather than hanging waiting on a
prompt that can't be answered. Public repositories are entirely unaffected — no keys, no ECDH,
same plaintext behavior as before.

### Granting access to existing objects: `rad lfs rekey`

Radicle's access control is retroactive: once a DID is authorized, it can replicate a
repository's entire history, including everything from before it was added. Without extra
handling, this encryption design would *not* match that: a newly-authorized collaborator could
see a private repo's full git history but be permanently unable to decrypt any LFS object
committed before they were granted access, since those objects' wrapped-key lists only include
whoever was authorized at the time.

`rad lfs rekey` closes that gap. Run by anyone *already* authorized (it can't be self-served by
the new collaborator — they have nothing to decrypt yet), it walks every encrypted object,
recovers each one's content key using the runner's own existing access, and adds a wrapped-key
entry for any currently-authorized DID that's missing one. It never re-encrypts content or
touches IPFS — it's a pure git-notes operation, so it's fast and doesn't need the original
file. Run it after adding a new delegate or allow-listing a new DID on a private repository, if
you want them to have access to previously-committed LFS objects (not automatic — see
[Prerequisites](#prerequisites)/[Use](#use) below for the full flow).

**Still explicitly not solved**: revocation. A formerly-authorized peer who already decrypted
an object can't be made to un-know its content key — the same fundamental limitation as
Radicle's own replication-based access control (already-replicated git objects can't be
un-seen either), not a regression introduced by this design.

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
git push rad                   # pushes your commit(s) and the refs/notes/rad-lfs mapping together
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

**On a private repository**, committing from a terminal you'll be prompted for your passphrase
if needed (see [Encryption for private repositories](#encryption-for-private-repositories)
above) — no extra setup required. After granting a new collaborator access
(`rad id update --allow <did>` or adding them as a delegate), have an already-authorized
collaborator run `rad lfs rekey` and push, so the new collaborator can decrypt
previously-committed LFS objects too, not just ones committed after they were added.

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
| Large file didn't get pinned (commit went through, but no `rad-lfs` note) | The pre-commit hook silently skips pinning if the `ipfs`/`rad` CLIs aren't on `PATH` — check `command -v ipfs` / `command -v rad`. Re-add the file and commit again once available. |
| `private-repo LFS operations need to perform a key-agreement (ECDH) operation ...` | Your keystore is encrypted, ssh-agent can't do key agreement (protocol limitation), and no passphrase is available — normally you'd just be prompted interactively; this only happens in a non-interactive context (no TTY, e.g. CI). Set `RAD_PASSPHRASE`, or use an unencrypted keystore, or run it from a terminal. |
| `you are not an authorized recipient of this object` | Either genuinely unauthorized (not a delegate/allow-listed DID for this private repo), or authorized *after* this specific object was committed and nobody has run `rad lfs rekey` since — ask an already-authorized collaborator to run it and push. |
| `fatal: couldn't find remote ref refs/notes/rad-lfs` on `git fetch`/`git pull` (breaks *all* fetching, not just LFS) | Fixed — but if this repo had `rad lfs init` run against an older build, re-run `rad lfs init` once to replace the stale, now-invalid fetch refspec it configured (see `NOTES-lfs-notes-fetch-bug.md` for the full story). |
| Plain `git push rad` (no branch name) doesn't push your commits, only the notes ref | Your repository was set up with an older build: `rad lfs init` used to only configure a push refspec for `refs/notes/rad-lfs`, not the branch, so a bare push covered only that. Fixed — re-run `rad lfs init` once (harmless, idempotent) to add the missing `+refs/heads/*:refs/heads/*` push refspec, then a bare `git push rad` pushes both together. Until then, push both explicitly: `git push rad <branch>` for the commit, then `git push rad` for the notes mapping. |
| Collaborator gets `no CID recorded for oid ...; the peer that has this object needs to push its refs/notes/rad-lfs ref` even though they can see your commit | You (or whoever committed the LFS file) pushed from a repository still missing the branch push refspec — see the row above. Re-run `rad lfs init`, push again, then have them `git fetch rad` (or re-clone/re-pull). |

## Status

This is a prototype, not upstream Radicle functionality. See the commits on the
`rad-lfs-ipfs` branch for the full change set relative to upstream `heartwood`.
