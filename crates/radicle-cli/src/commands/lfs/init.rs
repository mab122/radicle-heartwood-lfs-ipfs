//! `rad lfs init` — one-time, idempotent setup of Git LFS support for a
//! Radicle repository, backed by the contributor's local IPFS (Kubo) node.
//!
//! This deliberately does *not* talk to any seed-hosted HTTP LFS server:
//! large file content lives on each peer's own IPFS node, and the
//! oid -> CID mapping is carried in a git-notes ref (`refs/notes/rad-lfs`)
//! that replicates alongside the rest of the repository once the
//! push/fetch refspecs below are configured.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, anyhow};

use radicle::git::raw::Repository;

use crate::git;
use crate::ipfs::{self, LFS_NOTES_REF as NOTES_REF};
use crate::lfs_crypto::{LOCAL_NOTES_REF, NOTE_AUTHOR_EMAIL, NOTE_AUTHOR_NAME};
use crate::terminal as term;

/// Name under which our custom transfer agent is registered with Git LFS.
const TRANSFER_AGENT_NAME: &str = "rad-ipfs";

/// Binary implementing the custom transfer agent. Must be on the user's
/// `PATH`; we intentionally don't assume a specific install location.
const TRANSFER_AGENT_BIN: &str = "rad-lfs-transfer";

/// Name of the remote that `rad init` sets up.
const RAD_REMOTE: &str = "rad";

/// Markers delimiting the block we manage inside `.git/hooks/pre-commit`,
/// so re-running `rad lfs init` updates our block in place instead of
/// duplicating it, and so we don't clobber any hook content that was
/// already there.
const HOOK_BEGIN: &str = "# >>> rad-lfs-init managed pre-commit hook >>>";
const HOOK_END: &str = "# <<< rad-lfs-init managed pre-commit hook <<<";

/// Body of the managed pre-commit hook block (POSIX `sh`).
///
/// For every file staged in the commit that's tracked by Git LFS (i.e.
/// matches a `filter=lfs` pattern in `.gitattributes`), this computes the
/// file's LFS oid (sha256) and size, then feeds all of them to a single
/// `rad lfs precommit` invocation as "<oid> <size> <path>" lines on stdin.
/// `rad lfs precommit` does the actual IPFS add/pin (encrypting first for
/// private repos) and writes the corresponding notes on `refs/notes/rad-lfs`
/// itself — see `commands/lfs/precommit.rs` and `commands/lfs/store.rs`.
///
/// Batching into one `rad` process (rather than one per file) matters for
/// private repos specifically: each file needs the keystore passphrase for
/// an ECDH key-agreement operation, and a separate process per file meant a
/// separate passphrase prompt per file. One process loads it once.
///
/// The hook only computes the cheap, dependency-free oid/size values in
/// shell; the CID/encryption/git-notes logic lives in `rad lfs precommit`
/// since it needs access to the repo's identity/visibility and (for private
/// repos) key material that a plain shell script has no business handling —
/// a git hook shelling out to our own `rad` binary is far simpler than
/// reimplementing that here.
///
/// Requires both `ipfs` and `rad` to be available on `PATH` at commit time.
const HOOK_BODY: &str = r#"rad_lfs_precommit() {
    command -v ipfs >/dev/null 2>&1 || {
        echo "rad-lfs: 'ipfs' CLI not found on PATH; skipping CID pinning for this commit" >&2
        return 0
    }
    command -v rad >/dev/null 2>&1 || {
        echo "rad-lfs: 'rad' CLI not found on PATH; skipping CID pinning for this commit" >&2
        return 0
    }

    sha256() {
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$1" | awk '{ print $1 }'
        else
            shasum -a 256 "$1" | awk '{ print $1 }'
        fi
    }

    staged=$(mktemp) || return 1
    batch=$(mktemp) || { rm -f "$staged"; return 1; }
    # `-z` avoids git's default C-style quoting of non-ASCII filenames (e.g.
    # a literal "\305\202" for a Polish "ł") in `--name-only`'s normal
    # output, which would otherwise not match any real file on disk and
    # silently drop that file from LFS pinning. NUL-separated records are
    # converted to newline-separated here for a plain `read` loop, which is
    # safe since filenames practically never contain literal newlines.
    git diff --cached --name-only --diff-filter=ACM -z | tr '\0' '\n' > "$staged"

    while IFS= read -r file; do
        [ -n "$file" ] || continue
        [ -f "$file" ] || continue

        attr=$(git check-attr filter -- "$file" | sed -n 's/.*: filter: //p')
        [ "$attr" = "lfs" ] || continue

        oid=$(sha256 "$file") || { rm -f "$staged" "$batch"; exit 1; }
        size=$(wc -c < "$file" | tr -d ' ')

        printf '%s %s %s\n' "$oid" "$size" "$file" >> "$batch"
    done < "$staged"
    rm -f "$staged"

    if [ -s "$batch" ]; then
        rad lfs precommit < "$batch" || {
            echo "rad-lfs: 'rad lfs precommit' failed" >&2
            rm -f "$batch"
            exit 1
        }
    fi
    rm -f "$batch"
}

rad_lfs_precommit"#;

pub fn run() -> anyhow::Result<()> {
    let (repo, _rid) = radicle::rad::cwd()
        .context("`rad lfs init` must be run inside a Radicle repository working copy")?;
    let workdir = repo.workdir().ok_or_else(|| {
        anyhow!("`rad lfs init` must be run inside a git working copy (not a bare repository)")
    })?;

    ipfs::check_daemon()?;
    term::success!("Found a local IPFS (Kubo) daemon at {}", ipfs::kubo_api_url());

    ensure_git_lfs_available()?;
    git::git(workdir, ["lfs", "install", "--local"])
        .context("failed to run `git lfs install --local`")?;
    term::success!("Initialized Git LFS for this repository");

    git::git(
        workdir,
        ["config", "lfs.standalonetransferagent", TRANSFER_AGENT_NAME],
    )
    .context("failed to configure the LFS standalone transfer agent")?;
    git::git(
        workdir,
        [
            "config",
            &format!("lfs.customtransfer.{TRANSFER_AGENT_NAME}.path"),
            TRANSFER_AGENT_BIN,
        ],
    )
    .context("failed to configure the LFS custom transfer agent path")?;
    term::success!(
        "Configured Git LFS to transfer objects via `{TRANSFER_AGENT_BIN}` (backed by IPFS)"
    );

    // Push maps our *local-only* notes ref (`LOCAL_NOTES_REF`) to the bare
    // `NOTES_REF` on the remote: `git push` lands whatever local ref we
    // push under the pusher's own namespace on the server regardless of
    // the local ref name, the same way `refs/heads/*` does, so the
    // destination staying bare is correct and matches what
    // `list.rs`'s `lfs_notes_refs` looks for server-side. The *source*
    // is deliberately not the bare name too -- see `LOCAL_NOTES_REF`'s
    // doc comment for why (a git ref D/F conflict with fetched peer
    // refs).
    //
    // Fetch is different: `refs/notes/rad-lfs` is a per-peer ref (any
    // peer can commit an LFS-tracked file, not just delegates), so
    // there's no single canonical value the remote can advertise the way
    // it does for `refs/heads`/`refs/tags`. The remote instead advertises
    // every peer's note individually under `refs/notes/rad-lfs/<peer>`,
    // so the fetch refspec needs a wildcard to pull them all; the LFS
    // tooling merges across whatever's fetched (see
    // `lfs_crypto::find_envelope`) rather than expecting one ref.
    let notes_push_refspec = format!("+{LOCAL_NOTES_REF}:{NOTES_REF}");
    let fetch_refspec = format!("+{NOTES_REF}/*:{NOTES_REF}/*");
    let push_key = format!("remote.{RAD_REMOTE}.push");
    let fetch_key = format!("remote.{RAD_REMOTE}.fetch");

    // Migration: earlier versions of `rad lfs init` configured a plain,
    // non-wildcard fetch refspec for the notes ref, which `git fetch` can
    // never satisfy -- the remote never advertises a bare
    // `refs/notes/rad-lfs` (only the wildcarded per-peer form below), so
    // a stale entry here breaks the *entire* fetch, not just this one
    // ref. Remove it if present so re-running `rad lfs init` actually
    // fixes an already-configured repository, rather than leaving the
    // broken entry alongside the corrected one.
    remove_refspec_if_present(workdir, &fetch_key, &format!("+{NOTES_REF}:{NOTES_REF}"))?;
    // Migration: earlier versions also pushed from the bare local ref
    // (symmetric `+refs/notes/rad-lfs:refs/notes/rad-lfs`). Replace it
    // with the corrected asymmetric refspec above.
    remove_refspec_if_present(workdir, &push_key, &format!("+{NOTES_REF}:{NOTES_REF}"))?;

    ensure_refspec(workdir, &push_key, &notes_push_refspec)?;
    ensure_refspec(workdir, &fetch_key, &fetch_refspec)?;

    // As soon as *any* explicit push refspec is configured on a remote
    // (the notes one above), git stops falling back to its usual
    // push.default-driven behavior (pushing whatever branch you're on) for
    // a bare `git push rad` -- it only pushes what's explicitly listed.
    // Without a branch refspec too, that means a bare `git push rad` after
    // committing an LFS-tracked file would silently push *only* the notes
    // mapping, not the commit itself -- exactly the gotcha that caused a
    // collaborator to see "no CID recorded for oid ..." even though the
    // committer had "pushed". Configuring this explicitly, mirroring the
    // existing `+refs/heads/*:refs/remotes/rad/*` fetch refspec above,
    // makes a single bare `git push rad` push every local branch *and*
    // the notes mapping together.
    let heads_push_refspec = "+refs/heads/*:refs/heads/*".to_string();
    ensure_refspec(workdir, &push_key, &heads_push_refspec)?;

    term::success!(
        "Configured the `{RAD_REMOTE}` remote so a single `git push {RAD_REMOTE}` pushes your \
         branches and the `{NOTES_REF}` notes mapping together"
    );

    migrate_local_notes_ref(&repo)?;

    install_pre_commit_hook(&repo)?;
    term::success!("Installed the `rad-lfs` pre-commit hook");

    term::blank();
    term::success!("Git LFS is now configured for this repository, backed by your local IPFS node.");
    term::info!(
        "File CIDs are recorded in the `{NOTES_REF}` git-notes ref, which now travels with a plain `git push {RAD_REMOTE}` / `git pull {RAD_REMOTE}` alongside your branches."
    );

    Ok(())
}

/// Make sure the `git-lfs` binary is installed before we shell out to it,
/// so we can give a clear, actionable error instead of a raw "unknown git
/// command" failure.
fn ensure_git_lfs_available() -> anyhow::Result<()> {
    match Command::new("git-lfs").arg("version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(anyhow!(
            "`git-lfs version` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(anyhow!(
            "Git LFS is not installed (the `git-lfs` binary was not found on your PATH). \
             Install Git LFS — see https://git-lfs.com — and try again."
        )),
        Err(err) => Err(err).context("failed to check for `git-lfs`"),
    }
}

/// Add `value` to the (potentially multi-valued) git config key `key`,
/// unless it's already present, so re-running `rad lfs init` doesn't
/// duplicate refspecs.
fn ensure_refspec(workdir: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    // `git config --get-all` exits non-zero when the key is unset, which
    // isn't an error for us, so we use the lower-level `run` rather than
    // `git::git` (which treats a non-zero exit as failure).
    let output = radicle::git::run(Some(workdir), ["config", "--get-all", key])
        .with_context(|| format!("failed to read git config `{key}`"))?;
    let existing = String::from_utf8_lossy(&output.stdout);
    if existing.lines().any(|line| line.trim() == value) {
        return Ok(());
    }

    git::git(workdir, ["config", "--add", key, value])
        .with_context(|| format!("failed to set git config `{key}`"))?;
    Ok(())
}

/// Remove `value` from the (potentially multi-valued) git config key
/// `key`, if present, without disturbing any other values under the same
/// key (e.g. the default `+refs/heads/*:refs/remotes/rad/*` that plain
/// `git remote add` already configures on `remote.<rad>.fetch`).
fn remove_refspec_if_present(workdir: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let output = radicle::git::run(Some(workdir), ["config", "--get-all", key])
        .with_context(|| format!("failed to read git config `{key}`"))?;
    let existing = String::from_utf8_lossy(&output.stdout);
    if !existing.lines().any(|line| line.trim() == value) {
        return Ok(());
    }

    // `git config --unset` matches its value argument as a regex against
    // the whole existing value, not literally -- anchor and escape so it
    // removes exactly this one entry and nothing else.
    let escaped: String = value
        .chars()
        .map(|c| {
            if "\\^$.|?*+()[]{}".contains(c) {
                format!("\\{c}")
            } else {
                c.to_string()
            }
        })
        .collect();
    let pattern = format!("^{escaped}$");

    git::git(workdir, ["config", "--unset", key, &pattern])
        .with_context(|| format!("failed to remove stale git config `{key}` entry"))?;
    Ok(())
}

/// Migration: earlier versions of this fork wrote local notes directly to
/// the bare `NOTES_REF` (`refs/notes/rad-lfs`). Since the fetch refspec
/// populates `refs/notes/rad-lfs/<peer>` siblings locally -- including our
/// own peer's copy, fetched back after a push -- a leftover bare ref
/// causes a git ref D/F (file-vs-directory) conflict: `refs/notes/rad-lfs`
/// can't simultaneously be a leaf ref and a directory prefix for
/// `refs/notes/rad-lfs/<peer>` in the same namespace. That made every
/// `rad lfs store`/`rad lfs rekey` call after the first fetch fail
/// outright with "failed to write LFS note".
///
/// If a bare `refs/notes/rad-lfs` ref exists, copy its notes into
/// `LOCAL_NOTES_REF` (so nothing already recorded there is lost) and
/// delete the bare ref, eliminating the conflict. A no-op if the bare ref
/// doesn't exist (fresh setups, or a repo that's already been migrated).
fn migrate_local_notes_ref(repo: &Repository) -> anyhow::Result<()> {
    let Ok(mut reference) = repo.find_reference(NOTES_REF) else {
        return Ok(());
    };

    // Read every note out *before* touching anything -- writing to
    // `LOCAL_NOTES_REF` while the bare `NOTES_REF` still exists is the
    // exact same D/F conflict this migration exists to fix, just
    // self-inflicted. The bare ref has to be gone first.
    let mut migrated = Vec::new();
    if let Ok(notes) = repo.notes(Some(NOTES_REF)) {
        for (_, target_oid) in notes.flatten() {
            if let Ok(note) = repo.find_note(Some(NOTES_REF), target_oid)
                && let Some(message) = note.message()
            {
                migrated.push((target_oid, message.to_string()));
            }
        }
    }

    reference
        .delete()
        .context("failed to remove the legacy bare `refs/notes/rad-lfs` ref")?;

    if migrated.is_empty() {
        return Ok(());
    }

    let signature = radicle::git::raw::Signature::now(NOTE_AUTHOR_NAME, NOTE_AUTHOR_EMAIL)
        .context("failed to construct note signature")?;
    for (target_oid, message) in migrated {
        repo.note(&signature, &signature, Some(LOCAL_NOTES_REF), target_oid, &message, true)
            .with_context(|| {
                format!("failed to migrate note for {target_oid} to `{LOCAL_NOTES_REF}`")
            })?;
    }
    term::success!("Migrated local LFS notes from the legacy `{NOTES_REF}` ref");

    Ok(())
}

/// Install (or update, or chain onto) the `.git/hooks/pre-commit` hook that
/// pins staged LFS objects to IPFS and records their CID as a git note.
fn install_pre_commit_hook(repo: &Repository) -> anyhow::Result<()> {
    let hooks_dir = repo.path().join("hooks");
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("failed to create hooks directory `{}`", hooks_dir.display()))?;
    let hook_path = hooks_dir.join("pre-commit");

    let existing = fs::read_to_string(&hook_path).unwrap_or_default();

    let new_content = if let Some((before, after_begin)) = existing.split_once(HOOK_BEGIN) {
        // We've already installed our block before: replace it in place so
        // re-running `rad lfs init` doesn't duplicate it, while preserving
        // anything the user added before/after it.
        let after = after_begin
            .split_once(HOOK_END)
            .map_or("", |(_, after)| after);
        format!("{before}{HOOK_BEGIN}\n{HOOK_BODY}\n{HOOK_END}{after}")
    } else if existing.trim().is_empty() {
        format!("#!/bin/sh\n\n{HOOK_BEGIN}\n{HOOK_BODY}\n{HOOK_END}\n")
    } else {
        // Some other hook (not ours) is already installed (`git lfs
        // install` itself only installs a pre-push hook, so this is
        // usually a hook the user or another tool set up). Chain onto it
        // rather than clobbering it.
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(HOOK_BEGIN);
        content.push('\n');
        content.push_str(HOOK_BODY);
        content.push('\n');
        content.push_str(HOOK_END);
        content.push('\n');
        content
    };

    fs::write(&hook_path, new_content)
        .with_context(|| format!("failed to write hook `{}`", hook_path.display()))?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&hook_path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        fs::set_permissions(&hook_path, perms)?;
    }

    Ok(())
}
