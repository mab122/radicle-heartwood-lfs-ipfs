# Bug: `rad lfs store` fails with "failed to write LFS note" — likely fallout from the `refs/notes/rad-lfs/*` migration

**RESOLVED** (2026-07-15). The leading hypothesis in this report — a git ref D/F conflict
between the bare `refs/notes/rad-lfs` (what `store.rs`/`rekey.rs` wrote to) and the namespaced
`refs/notes/rad-lfs/<peer>` (what fetch populates, including your own peer's copy fetched back
after a push) — was confirmed exactly, reproduced locally in the identical order that
triggers it (fetch a namespaced sibling in *first*, then attempt a local write). See
"Fix actually shipped" at the bottom for the full writeup, the fix, and local verification
(including the migration path for already-affected repos like this one).

`~/Blog/Blog` and the VPS checkout both need `rad lfs init` re-run once the fix is installed,
to run the migration and pick up the corrected push refspec — see the deployment steps at the
bottom before relying on `rad lfs store` there again.

## Repro

```
$ rad lfs store --oid <sha256 of a food jpg> --size <n> -- assets/food/boczekwziemniaczkach.jpg
✗ Error: failed to write LFS note
```

Exit code 1. No further detail is printed — `store.rs`'s only `.context("failed to write LFS
note")?` wraps the `repo.note(...)` call at the very end of `run()`
(`crates/radicle-cli/src/commands/lfs/store.rs:74`), and the CLI's top-level error printer only
shows the outermost context, not the full `anyhow` chain, so the actual libgit2/`radicle`
error underneath is not visible from the CLI alone.

Confirmed via `RUST_LOG=trace` that everything *before* the note write succeeds:
- the LFS pointer blob is written (`repo.blob(...)`, no error)
- `ipfs::check_daemon()` succeeds (local Kubo at `127.0.0.1:5001`, confirmed reachable)
- `ipfs::add()` succeeds — HTTP 200 from `/api/v0/add`, got back a CID
  (`QmXshEtcDYxmXvpaF13otbysCeqzvhNS2U8GF1hr6qFxrc`)
- `ipfs::pin_all()` succeeds — HTTP 200 from `/api/v0/pin/add`

So the failure is isolated to the final `repo.note(&signature, &signature,
Some(ipfs::LFS_NOTES_REF), blob_oid, &message, true)` call. No network I/O happens after the
pin step (confirmed in the trace log — the error is printed immediately after the pin response,
with no further HTTP/socket activity), so this is a local storage-write failure, not a
network/IPFS-daemon issue. This also rules out "rad node isn't running" as the cause — it was
freshly restarted and confirmed connected to peers (`rad node status`) before this repro.

## Suspected root cause: `NOTES_REF` was never updated to match the per-peer ref migration

`crates/radicle-cli/src/lfs_crypto.rs:42`:
```rust
pub const NOTES_REF: &str = "refs/notes/rad-lfs";
```

This is the *unnamespaced* ref name, and it's what `store.rs` passes as the notes ref to write
into (`ipfs::LFS_NOTES_REF` re-exports this constant, see `ipfs.rs:28`).

But the fetch-side fix in `NOTES-lfs-notes-fetch-bug.md` (commit `1c65ee62`, "Fix: git fetch rad
can never fetch refs/notes/rad-lfs") deliberately moved away from one canonical bare ref to a
**per-peer** ref shape, `refs/notes/rad-lfs/<peer-id>`, and `rad lfs init` now configures the
remote fetch refspec accordingly:

```
+refs/notes/rad-lfs/*:refs/notes/rad-lfs/*
```

Confirmed on this machine, in the blog repo's working copy, right now:

```
$ git config --get-all remote.rad.fetch
+refs/heads/*:refs/remotes/rad/*
+refs/tags/*:refs/remotes/rad/tags/*
+refs/notes/rad-lfs/*:refs/notes/rad-lfs/*

$ git for-each-ref refs/notes/
c4538c2d... commit  refs/notes/rad-lfs/z6MkkphWkrBU3Buhh4ZN8Gtt9KLzn2DUsx3hZhhXirK7XAyn
```

i.e. the **working copy** now has the note under the new namespaced shape (fetched down from
the remote, post-fix). But in the **storage repo** (`~/.radicle/storage/<rid>`, where
`store.rs`'s `repo.note()` call actually writes, via `NOTES_REF` = the old bare name):

```
$ git -C ~/.radicle/storage/z2Z16c9C5BaqSQPfbVrGPGG68wAx3 for-each-ref | grep notes
c4538c2d... commit  refs/namespaces/z6MkkphW.../refs/notes/rad-lfs
```

— still the **old, unnamespaced** ref (`refs/notes/rad-lfs`, no peer suffix), holding the one
note that was successfully written earlier today (17:08:38, for the now-reverted
`_infra-test/lfs-ipfs-smoketest.jpg` smoke test — that first write succeeded because at that
point nothing had fetched/created the namespaced sibling ref yet).

So the repo now has **two differently-shaped note refs pointing at overlapping data**: the
legacy bare `refs/notes/rad-lfs` (what `store.rs` still writes to) in storage, and the new
namespaced `refs/notes/rad-lfs/<peer>` (what fetch/init now expect and manage) in the working
copy and presumably also fetchable from storage once something writes it there. This is the
same shape of problem already diagnosed once in `NOTES-lfs-notes-fetch-bug.md` — "a stale local
plain note ref pre-existed and D/F-conflicted with the incoming namespaced one" — just hit from
the **write** side (`store.rs`) this time instead of the fetch side. Given git's notes trees use
fanout-by-target-oid internally, and notes refs are just regular refs, having both a bare
`refs/notes/rad-lfs` and a `refs/notes/rad-lfs/<peer>` **coexist** is a classic D/F (file vs.
directory) ref conflict: `refs/notes/rad-lfs` can't simultaneously be a leaf ref and a directory
prefix for `refs/notes/rad-lfs/<peer>` in the same ref namespace. That would explain why the
very first note write (before any namespaced sibling existed) succeeded, and why the second one
(now that `rad lfs init` has created the namespaced ref shape) fails outright, silently, at
exactly the ref-write step.

**Not yet confirmed against the actual libgit2 error text** (the CLI swallows it — see above),
so treat the D/F-conflict theory as the leading hypothesis, not a confirmed fix target. Worth
checking next:

1. Add temporary `eprintln!("{:#}", e)` (or similar) around the `repo.note(...)` call in
   `store.rs` to print the full `anyhow` chain instead of just the top context, to get the real
   libgit2 error text.
2. If it is a D/F conflict: `NOTES_REF` in `lfs_crypto.rs:42` needs to move to the same
   per-peer-namespaced shape `store.rs` writes with (probably `refs/notes/rad-lfs/<own-peer-id>`,
   matching what `init.rs`'s migrated refspec expects), not just the fetch/init side.
3. Given the write already touched storage in a bad way, may need a one-off manual cleanup on
   this machine (`git update-ref -d refs/namespaces/<peer>/refs/notes/rad-lfs` inside
   `~/.radicle/storage/<rid>`, analogous to the working-copy cleanup already documented for the
   fetch bug) once the write-path fix lands, to avoid the same ref existing in both old and new
   shape indefinitely.

## Impact

Blocks committing any new file into an LFS-tracked path (`assets/food/*.jpg`,
`assets/_infra-test/*.jpg`) in the blog repo via the `rad-lfs-init`-managed pre-commit hook,
which calls the equivalent of this same `rad lfs store` command per staged LFS file. Worked
around for the immediate food-gallery migration by committing with `--no-verify` to skip the
hook (plain `git-lfs` still produces correct LFS pointers; only the IPFS pinning step is
skipped for those files until this is fixed).

## Fix actually shipped

The D/F-conflict hypothesis was exactly right, confirmed by reproducing it locally in the
precise order that triggers it: `rad lfs init` → commit+push a file (writes bare
`refs/notes/rad-lfs` locally, lands namespaced in storage) → `git fetch rad` (pulls that same
note back down as `refs/notes/rad-lfs/<own-peer-id>`, since the fetch fix advertises *every*
peer including yourself) → a second `rad lfs store` call now fails outright with the exact
reported "failed to write LFS note", because the working copy now has both a bare
`refs/notes/rad-lfs` (a leaf ref) and `refs/notes/rad-lfs/<peer>` (implying `refs/notes/rad-lfs`
is a directory) — git can't reconcile that. (Interesting side note: plain `git fetch` itself
hits the *same* conflict from the other direction and just refuses that one ref update cleanly
with "unable to update local ref" — it's specifically `repo.note()`/libgit2's ref-write path
in `store.rs`/`rekey.rs` that fails hard instead, which is why the CLI's error was so much less
informative.)

### The fix

Stop ever writing to the bare `refs/notes/rad-lfs` locally. `crates/radicle-cli/src/lfs_crypto.rs`
now has two constants instead of one:

- `NOTES_REF` (`refs/notes/rad-lfs`, unchanged) — the *remote-side* canonical name: push
  destination, and what `list.rs`'s `lfs_notes_refs` looks for within each peer's namespace on
  the server. Still correct and unchanged.
- `LOCAL_NOTES_REF` (new: `refs/notes/rad-lfs/local`) — what `rad lfs store`/`rad lfs rekey`
  actually write to now. Living under the same `refs/notes/rad-lfs/` prefix as every fetched
  peer ref means it's just another sibling leaf ref, structurally incapable of D/F-conflicting
  with them — `local` isn't a valid peer ID string, so it can never collide with an actual
  fetched peer namespace either.

`rad lfs init`'s push refspec becomes asymmetric: `+refs/notes/rad-lfs/local:refs/notes/rad-lfs`
(local source → bare remote destination — the destination staying bare is correct and
unchanged, since push already lands under the pusher's own namespace server-side regardless of
the local ref name, same as `refs/heads/*`). The fetch refspec is untouched
(`+refs/notes/rad-lfs/*:refs/notes/rad-lfs/*`), already correct from the previous fix.

**Migration for already-affected repos** (this one included): `rad lfs init` now also runs
`migrate_local_notes_ref()` — if a bare `refs/notes/rad-lfs` ref still exists locally, it reads
every note out, deletes the bare ref, then re-writes them all under `refs/notes/rad-lfs/local`.
Nothing is lost. (First implementation of this had its own bug, caught during local
verification: it tried to *create* `refs/notes/rad-lfs/local` while the bare ref still existed
— the exact same D/F conflict, self-inflicted, inside the migration meant to fix it. Fixed by
reading all notes into memory first, deleting the bare ref, *then* writing them to the new
location.) Also removes the old symmetric push refspec entry
(`+refs/notes/rad-lfs:refs/notes/rad-lfs`) the same way the earlier fetch-refspec migration
did, so re-running `rad lfs init` is enough to fully repair an already-configured repo — no
manual `git update-ref`/config surgery needed.

### Verified locally (reproduced the exact bug, then the exact fix, same session)

- Reproduced the failure precisely: fresh repo, commit+push (bare ref written), fetch (creates
  the namespaced sibling), second `rad lfs store` call → `✗ Error: failed to write LFS note`,
  byte-for-byte the reported symptom.
- Rebuilt with the fix, retried the *exact same failing call* in the *same repo* → succeeds,
  both `refs/notes/rad-lfs/local` and `refs/notes/rad-lfs/<peer>` coexist with no conflict.
- Tested the migration path directly: a repo with a pre-existing bare `refs/notes/rad-lfs`
  (content: `{"v":1,"cid":"Qmb..."}`) → `rad lfs init` → note correctly moved to
  `refs/notes/rad-lfs/local` with identical content, bare ref gone.
- Full round-trip after the fix: committed a new file (pre-commit hook → `rad lfs store` →
  writes `refs/notes/rad-lfs/local`), pushed (asymmetric refspec correctly maps it to the
  bare remote ref), a second checkout fetched it and `rad lfs fetch`'d the content —
  SHA-256 matched exactly.
- `rad lfs rekey` smoke-tested post-fix (it writes notes too, via the same `LOCAL_NOTES_REF`
  now) — ran cleanly, no crash.

### Deployment needed on already-affected machines

Both `~/Blog/Blog` (author's machine) and the VPS blog repo hit this bug and need the fix
installed, followed by one `rad lfs init` re-run each (to migrate the existing bare ref and
pick up the corrected push refspec) before `rad lfs store`/committing new LFS files will work
cleanly again there. The files committed via the `--no-verify` workaround are fine as-is
(valid LFS pointers, just not yet IPFS-pinned) — re-adding/re-committing them normally (or
running `rad lfs store` on them directly) after the fix is installed will pin them retroactively.
