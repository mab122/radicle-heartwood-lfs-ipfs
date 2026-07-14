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
/// matches a `filter=lfs` pattern in `.gitattributes`), this:
///  1. `ipfs add`s the real file content to the local Kubo daemon,
///  2. `ipfs pin add`s the resulting CID,
///  3. computes the git blob hash of the LFS pointer text for that file, and
///  4. writes a `cid=<cid>` git note on that blob hash under
///     `refs/notes/rad-lfs`.
///
/// This mirrors `radicle-lfs-server`'s `notes::pointer_blob_hash` /
/// `notes::write_cid` logic (see that crate's `src/notes.rs`), reimplemented
/// here as a shell script since a git hook has no business depending on a
/// separate Rust crate.
const HOOK_BODY: &str = r#"rad_lfs_precommit() {
    command -v ipfs >/dev/null 2>&1 || {
        echo "rad-lfs: 'ipfs' CLI not found on PATH; skipping CID pinning for this commit" >&2
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
    git diff --cached --name-only --diff-filter=ACM > "$staged"

    while IFS= read -r file; do
        [ -n "$file" ] || continue
        [ -f "$file" ] || continue

        attr=$(git check-attr filter -- "$file" | sed -n 's/.*: filter: //p')
        [ "$attr" = "lfs" ] || continue

        oid=$(sha256 "$file") || { rm -f "$staged"; exit 1; }
        size=$(wc -c < "$file" | tr -d ' ')

        cid=$(ipfs add -Q -- "$file") || {
            echo "rad-lfs: 'ipfs add' failed for $file" >&2
            rm -f "$staged"
            exit 1
        }
        ipfs pin add -- "$cid" >/dev/null || {
            echo "rad-lfs: 'ipfs pin add' failed for $cid" >&2
            rm -f "$staged"
            exit 1
        }

        blob_sha=$(printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize %s\n' "$oid" "$size" | git hash-object --stdin -t blob) || {
            rm -f "$staged"
            exit 1
        }

        git notes --ref=refs/notes/rad-lfs add -f -m "cid=$cid" "$blob_sha" >/dev/null || {
            echo "rad-lfs: failed to write git note for $file" >&2
            rm -f "$staged"
            exit 1
        }
    done < "$staged"

    rm -f "$staged"
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

    let notes_refspec = format!("+{NOTES_REF}:{NOTES_REF}");
    ensure_refspec(workdir, &format!("remote.{RAD_REMOTE}.push"), &notes_refspec)?;
    ensure_refspec(workdir, &format!("remote.{RAD_REMOTE}.fetch"), &notes_refspec)?;
    term::success!(
        "Configured the `{RAD_REMOTE}` remote to push/fetch the `{NOTES_REF}` notes ref"
    );

    install_pre_commit_hook(&repo)?;
    term::success!("Installed the `rad-lfs` pre-commit hook");

    term::blank();
    term::success!("Git LFS is now configured for this repository, backed by your local IPFS node.");
    term::info!(
        "File CIDs are recorded in the `{NOTES_REF}` git-notes ref, which now travels with every `rad push` / `rad pull`."
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
