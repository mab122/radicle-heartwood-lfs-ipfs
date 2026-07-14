# Bug: `git fetch rad` can never fetch `refs/notes/rad-lfs`

**RESOLVED** (2026-07-14). Fix: `refs/notes/rad-lfs` is now exposed per-peer
(`refs/notes/rad-lfs/<peer-id>`) instead of expecting one canonical bare ref, matching how
patch refs already work — see "Fix actually shipped" at the bottom for the full writeup and
verification. Kept the original report below unedited for context.

Found while deploying `rad-lfs-ipfs` to mbator.pl for the blog pipeline (2026-07-14).
Blocks the whole LFS-over-IPFS flow end-to-end, not just a cold-start edge case.

## Symptom

After `rad lfs init` in any repo, `git fetch rad` (and therefore `git pull`, and anything
that does the equivalent, e.g. `deploy.sh`'s `git fetch rad && git reset --hard rad/master`)
fails every time with:

```
fatal: couldn't find remote ref refs/notes/rad-lfs
```

This isn't specific to a fresh repo where nobody has pushed an LFS object yet — it
reproduces even after a real push has created the note and it's fully present in the
receiving node's storage.

## Root cause

Two places are involved:

1. **`crates/radicle-cli/src/commands/lfs/init.rs:134`** — `rad lfs init` configures the
   client-side fetch/push refspec as a bare, non-namespaced ref:
   ```rust
   let notes_refspec = format!("+{NOTES_REF}:{NOTES_REF}");  // +refs/notes/rad-lfs:refs/notes/rad-lfs
   ```

2. **`crates/radicle-remote-helper/src/list.rs`, `for_fetch()`** (lines 38-79) — this is
   what actually determines which refs `git-remote-rad` advertises as fetchable for a
   plain (non-namespaced) `git fetch rad`. It only ever lists:
   - `HEAD`
   - `refs/heads/*` and `refs/tags/*` (canonicalized across delegates)
   - patch refs (via `patch_refs()`)

   It has **no knowledge of `refs/notes/rad-lfs` at all**. Notes only exist per-peer,
   under `refs/namespaces/<peer-id>/refs/notes/rad-lfs` (confirmed directly on a live
   node: `git for-each-ref` in the node's storage shows
   `refs/namespaces/z6Mkkph.../refs/notes/rad-lfs`, never a bare `refs/notes/rad-lfs`).
   `refs/heads/*` gets a canonical, non-namespaced view via delegate-threshold
   resolution; `refs/notes/rad-lfs` has no equivalent resolution step, so it's simply
   never in the list the remote helper offers — regardless of what refspec the client
   has configured in `.git/config`.

So the client asks for a ref the server-side listing never advertises, and `git fetch`
(which validates requested refspecs against the advertised ref list) fails hard for the
*entire* fetch, not just that one ref — meaning a repo with `rad lfs init` run can't
`git fetch`/`git pull` *anything* anymore, heads included, until this is fixed.

## Suggested direction for a fix

`for_fetch()` needs to expose some resolved form of the LFS notes ref for the
non-namespaced case, analogous to how `refs/heads/*` gets canonicalized. Options,
roughly in order of how much they change the design:

- **Simplest**: expose *all* peers' notes refs individually under some fetchable name
  (e.g. `refs/notes/rad-lfs/<peer-id>`), and change the LFS tooling (`rad lfs
  store`/`fetch`/`rekey`) to read/merge across all of them instead of expecting one
  canonical ref. Matches how git notes are designed to be merged anyway.
- **Closer to current design**: pick one canonical note per commit the same way heads
  get a canonical value (e.g. delegate-threshold agreement, or "the delegate's own
  namespace" the way `for_push` already restricts pushes to `profile.id()`'s own
  namespace) and expose that as the bare `refs/notes/rad-lfs`.
- Either way, `init.rs`'s refspec (`+refs/notes/rad-lfs:refs/notes/rad-lfs`) is probably
  fine as-is *once* the server side actually advertises something at that path — the
  bug is really in `list.rs`, not `init.rs`.

## Confirmed working around this bug (verified live on mbator.pl, 2026-07-14)

Everything upstream and downstream of this one step works:
- Fork built clean (`cargo install` for `radicle-cli`, `radicle-node`,
  `radicle-remote-helper`, `radicle-lfs-transfer`) at commit `33ffa122`.
- `rad lfs init` succeeds and configures the pre-commit hook, transfer agent, and (broken)
  refspec correctly per its own logic.
- A real private-repo LFS push (`git push rad master`, encrypted via ECDH, `RAD_PASSPHRASE`
  from an ssh-agent-backed key) succeeded and replicated to a seed node.
- Node-to-node replication of the note itself works fine at the storage layer — the
  receiving node's `refs/namespaces/<pusher>/refs/notes/rad-lfs` is populated correctly.
- The failure is *only* in the working-copy-level `git fetch rad` being unable to see it.

## Also found along the way (separate, minor)

Two `radicle-cli` crashes (not yet investigated, crash reports written to `/tmp/report-*.toml`
on the VPS at the time):
- `rad node status` crashed once (worked on retry).
- `rad seed` (no args, to check a repo's local seeding policy) crashed consistently.

## Deployment state left in place (mbator.pl)

- `blog` user has a working IPFS (Kubo) container, the fork built at `~/.radicle/bin`,
  `radicle-node-blog.service`/`blog-radicle-watch.service`/`blog-deploy.service` all
  pointed at the new binaries with `PATH`/`KUBO_API_URL` set correctly.
- Added an explicit `connect` entry for the local `seed` node
  (`z6MkmUF9KWFBQzYXBeNJk1bXXQGzaATnEsWxmbUYt54fyEC6@127.0.0.1:8776`) to
  `/home/blog/.radicle/config.json` — the private `blog` node had no route to the local
  public seed by default (`connect: []`, generic `preferredSeeds`), so `rad sync` timed
  out until this was added. Worth keeping regardless of the notes bug.
- Public `seed` node/`/usr/bin/radicle-node`/`radicle-httpd` untouched throughout.
- A harmless test commit (`3f650d2`, "infra: LFS-over-IPFS smoke test (safe to remove)")
  is sitting on top of the blog repo's real history in `~/Blog/Blog` and pushed to the
  network — adds `assets/_infra-test/lfs-ipfs-smoketest.jpg` (LFS-tracked, not referenced
  from any post, won't appear anywhere on the live site). Safe to leave or revert
  whenever convenient.

## Next step once fixed

Re-run `git fetch rad` in `/home/blog/repo` on the VPS (as `blog`) — if the fix resolves
the notes ref, that fetch (and the LFS smudge it triggers) is the last remaining piece to
confirm the whole IPFS content-fetch path end-to-end.

## Fix actually shipped

Went with the "simplest" option from the list above, since it's also the semantically
correct one: `refs/heads`/`refs/tags` get a canonical, non-namespaced value because only
*delegates'* view matters for canonical history — a whole separate subsystem (canonical-ref
computation on push/sync) maintains that. Notes have no equivalent: any peer, not just
delegates, can commit an LFS-tracked file and write a note for it, so there's no single
"correct" value to resolve to. Building delegate-consensus canonicalization for notes would
also have been semantically wrong, not just more work — it would silently drop non-delegate
contributors' LFS objects from ever being discoverable by anyone else.

- `crates/radicle-remote-helper/src/list.rs`, `for_fetch()`: now also lists every peer's
  `refs/notes/rad-lfs`, individually, as `refs/notes/rad-lfs/<peer-id>` (new `lfs_notes_refs`
  helper, using `stored.remotes()` + `stored.reference_oid()`). `for_push()` needed no change
  — turns out it's purely informational (used for the `list for-push` git protocol handshake,
  i.e. fast-forward computation), not an enforcement gate; the actual `push_ref`/`send-pack`
  call already accepts any refspec regardless of what `for_push()` lists, which is why notes
  pushing already worked despite `for_push()` only ever listing `refs/heads`/`refs/tags`.
- `crates/radicle-cli/src/commands/lfs/init.rs`: fetch refspec becomes a wildcard
  (`+refs/notes/rad-lfs/*:refs/notes/rad-lfs/*`); push refspec is unchanged (`git push`
  lands under the pusher's own namespace server-side regardless of the local ref name, same
  as `refs/heads/*`, so no wildcard needed there). Added a migration step
  (`remove_refspec_if_present`) that strips the old broken non-wildcard fetch refspec from
  already-configured repos — otherwise re-running `rad lfs init` would've just added the
  correct wildcard *alongside* the still-broken exact-match entry, which would still break
  the whole fetch (git fails the entire fetch if *any* requested refspec isn't advertised).
- `crates/radicle-cli/src/lfs_crypto.rs`: new `find_envelopes`/`all_note_targets`/`find_cids`
  read across every notes ref (local + every fetched peer), not just one. Notes sharing the
  same `cid` (e.g. an original commit plus a later `rekey` that added recipients under a
  *different* peer's namespace) have their recipient lists safely unioned. Notes with
  *different* `cid`s for the same object (only possible if two peers independently encrypted
  byte-identical content — same oid/size — producing different ciphertext each time, since
  encryption uses a fresh random key/nonce) are kept as separate candidates; `rad lfs
  fetch`/`rekey` try each in turn rather than assuming there's only ever one.
- `crates/radicle-cli/src/commands/lfs/fetch.rs`, `rekey.rs`, `ipfs.rs` (`lfs_cids`, used by
  `seed`/`unseed` pinning): all switched from single-ref `repo.notes(Some(NOTES_REF))` /
  `find_note` calls to the new multi-ref helpers above. `store.rs` needed no read-side change
  (write-only, always writes to the local plain ref, which is correct as-is).

### Verified for real (not the storage-copy shortcut used for the base feature's original
tests — this bug lives specifically in the `git-remote-rad` transport, so the fix had to be
exercised through the actual thing)

Built fresh `rad`/`git-remote-rad`/`radicle-node`/`rad-lfs-transfer`, created two-then-three
real Radicle identities/profiles, a real private repo with an allow-list, and used the
**real** `rad://` remote (`git remote add rad rad://<rid>` for fetch, matching exactly what
`rad init` itself configures — an earlier test attempt using a peer-scoped URL for both fetch
and push accidentally bypassed the bug entirely by routing into `for_fetch()`'s *namespaced*
branch, which was never broken; had to redo it with the correct canonical URL to actually
exercise the bug/fix).

- `git ls-remote rad` on the canonical URL now advertises `refs/notes/rad-lfs/<peer>` instead
  of nothing.
- A genuine `git fetch rad` (no explicit refspec workaround) succeeds and actually pulls the
  peer's notes ref down locally — this is the exact command from the bug report that used to
  fail with `fatal: couldn't find remote ref refs/notes/rad-lfs`.
- `git lfs pull` after that fetch correctly decrypts the private object end-to-end
  (SHA-256-verified).
- Re-tested the rekey scenario (grant a new peer access after the object was already
  committed) through the same real fetch path: denied before rekey, an authorized peer runs
  `rad lfs rekey` + pushes, the new peer re-fetches and successfully decrypts.

### Also discovered along the way (separate, not fixed here — flagging, not scope-creeping)

Once `rad lfs init` adds *any* explicit `remote.<rad>.push` refspec (the notes one), a plain
`git push rad` (no branch argument) stops falling back to `push.default=simple` for the
current branch — git only pushes whatever's explicitly configured once any explicit refspec
exists. So after running `rad lfs init`, `git push rad` alone only pushes the notes ref;
`git push rad master` (explicit) is needed to also push the branch. This isn't new — it was
already true before this fix, and both the original bug report's own reproduction and this
fix's testing always used an explicit branch name when pushing, so it never surfaced as a
problem in practice. Worth a real fix at some point (e.g. `rad lfs init` also ensuring an
explicit `+refs/heads/*:refs/remotes/rad/*`-equivalent push default is present, or documenting
"always `git push rad <branch>` explicitly" as a hard requirement), but out of scope for this
specific fetch-bug fix.
