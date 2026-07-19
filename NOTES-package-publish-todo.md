# Status: 10MB IPFS `cat` fix — code done and pushed; package build/publish still needs your input

## Done (verified, no further action needed)

- Root-caused and fixed the bug: `crates/radicle-cli/src/ipfs.rs`'s `cat()` used ureq's
  `Body::read_to_vec()`, which silently caps reads at 10MB (a default meant for typical JSON
  API responses, not LFS content). Any LFS object over 10MB failed with "the response body is
  larger than request limit: 10485760" — this is exactly what broke `blog-deploy.service` on
  the VPS today, on `assets/posts/hashcash/thumbnail.xcf` (29MB).
- Fix: raised the limit to 1GiB via ureq's `.with_config().limit(...)`. Commit `35f1f972`,
  branch `rad-lfs-ipfs`.
- Verification performed:
  1. Added a `#[cfg(test)]` regression test (`ipfs::tests::cat_round_trips_content_larger_than_ureqs_default_10mb_limit`,
     `#[ignore]`d since it needs a live Kubo daemon) — a 15MB add+cat round-trip.
  2. Confirmed it **fails** with the exact production error when the fix is reverted, and
     **passes** with the fix in place.
  3. Ran the full `radicle-cli` test suite (123 tests) — one pre-existing, unrelated failure
     (`rad_help`, a stale `--help`-output snapshot missing the `lfs` subcommand — confirmed to
     fail identically on the unmodified base commit, nothing to do with this change).
  4. `cargo clippy` clean (one pre-existing warning in an unrelated file/crate).
  5. **Live reproduction against the real file**: moved aside the cached LFS blob for
     `thumbnail.xcf` in `~/Blog/Blog`, ran `rad lfs fetch` with the *old* installed binary —
     reproduced the identical production error byte-for-byte. Ran it again with the *newly
     built* binary — the 10MB error is completely gone; it now correctly proceeds to the next
     step (prompting for a passphrase to decrypt the private-repo content, which is expected
     behavior and unrelated to this bug — I did not attempt to supply a passphrase). Restored
     the blog repo's `.git/lfs/objects` cache back to its original state afterward; no changes
     left in `~/Blog/Blog`.
- Pushed to `origin` (the Forgejo remote, `ssh://git.hswro.org:9022/mab122/radicle-heartwood-lfs.git`),
  branch `rad-lfs-ipfs`, commit `35f1f972..4ae9756a`.

## Not done — needs your call

I did **not** build or publish a package. Stopping here rather than guessing, because:

1. **No existing Forgejo-package publishing path in this repo.** There's a `build/Dockerfile`
   that cross-compiles reproducible `.tar.xz` releases (4 targets: linux/macOS ×
   x86_64/aarch64) via `cargo-zigbuild`, but nothing that uploads anywhere — no CI workflow,
   no `curl`-to-package-registry script, nothing referencing Forgejo's package API. I don't
   know what package type you want registered (generic package? a distro-specific one?), what
   name/version scheme you use there, or the auth (token?) needed to publish to
   `git.hswro.org`'s package registry. I deliberately did not go hunting through credential
   stores (`~/.netrc`, etc.) to find one myself — that's exactly the kind of thing I should ask
   about rather than dig for.
2. **The existing Dockerfile's package doesn't include `rad-lfs-transfer`** — only `rad`,
   `git-remote-rad`, and `radicle-node` get copied into the release tarball. `rad-lfs-transfer`
   is a separate crate/binary (`radicle-lfs-transfer/`) and is required for this fork's whole
   LFS-over-IPFS feature to work on a fresh install — worth fixing in the Dockerfile too, but
   I didn't want to change the packaging script's scope without confirming that's wanted.
3. **Platform scope is unclear.** The existing Dockerfile builds all 4 targets (needed for a
   real public release); the VPS only needs `x86_64-unknown-linux-musl`. Building all 4 takes
   meaningfully longer and needs the macOS SDK blob (`build/macos-sdk-11.3.tar.xz`) and Zig
   cross-toolchain set up — I can do it, but wanted to confirm whether you want the full
   release matrix or just what's needed to update the VPS.

If you want me to keep going on this (rather than picking it up in the session that already
knows this fork's packaging conventions), I'd need: the Forgejo package type/target and an
auth token (however you'd like to provide it — e.g. via an env var you set, not me searching
for it), confirmation on whether to add `rad-lfs-transfer` to the Dockerfile's package list,
and whether to build the full 4-target matrix or just linux/x86_64.
