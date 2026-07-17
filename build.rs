use std::env;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Without this, cargo has no way to know it needs to re-run this
    // script just because the current commit changed -- by default it
    // only reruns a build script when a file inside the crate's own
    // source tree changes, and `.git` is outside that entirely. That
    // silently let GIT_HEAD/RADICLE_VERSION go stale on ordinary
    // incremental rebuilds across commits that didn't happen to touch
    // this crate's own sources (discovered live: several `cargo build`s
    // in a row kept reporting an old commit's version despite the
    // working tree being clean at a newer commit). `.git/logs/HEAD` (the
    // reflog) is touched on every commit/checkout/merge, unlike
    // `.git/HEAD` itself, which only changes when you switch branches.
    println!("cargo::rerun-if-changed=../../.git/logs/HEAD");

    // Set a build-time `GIT_HEAD` env var which includes the commit id;
    // such that we can tell which code is running.
    let hash = env::var("GIT_HEAD").unwrap_or_else(|_| {
        Command::new("git")
            .arg("rev-parse")
            .arg("--short")
            .arg("HEAD")
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or("unknown".into())
    });

    let version = env::var("RADICLE_VERSION").unwrap_or_else(|_| {
        // If `RADICLE_VERSION` is not set, we still try our best to
        // describe this version by asking git. The result will in many
        // cases be a reference to the last released version, and how
        // many commits we are ahead, plus a short version of the
        // object ID of `HEAD`, e.g. `releases/x.y.z-80-gefe10f95be-dirty`
        // which would mean that we built 80 commits ahead of release
        // x.y.z, with efe10f95be being a unique prefix of the OID of
        // `HEAD`, and the working directory was dirty.
        // If this is a build pointing to a commit that has release tag, this
        // will just return the tag name itself, e.g. `releases/x.y.z`.
        // If all fails, we just use `hash`, which, in the worst case is
        // still "unknown" (see above) but in most cases will just be
        // the short OID of `HEAD`.
        Command::new("git")
            .arg("describe")
            .arg("--always")
            .arg("--broken")
            .arg("--dirty")
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or(hash.clone())
    });

    // Since in the previous step we are likely to almost always end up with
    // a prefix of `releases/`, as this is the scheme we use in this
    // repository, we remove this common prefix, to get nice version numbers.
    let version = if let Some(stripped) = version.strip_prefix("releases/") {
        stripped.to_owned()
    } else {
        version
    };

    // This is the `rad-lfs-ipfs` fork (Git LFS support backed by IPFS, see
    // README.md/LFS-IPFS.md) -- append a semver build-metadata suffix (the
    // `+...` part a `git describe`-derived version doesn't otherwise have
    // room for) so `--version` unambiguously identifies a binary as this
    // fork rather than upstream heartwood, e.g. when checking what a
    // deploy target is actually running. Applies to every binary in this
    // workspace, since they all share this build script via symlink.
    let version = format!("{version}+lfs-ipfs");

    // Set a build-time `SOURCE_DATE_EPOCH` env var which includes the commit time.
    let commit_time = env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| {
        Command::new("git")
            .arg("log")
            .arg("-1")
            .arg("--pretty=%ct")
            .arg("HEAD")
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or(0.to_string())
    });

    println!("cargo::rustc-env=RADICLE_VERSION={version}");
    println!("cargo::rustc-env=SOURCE_DATE_EPOCH={commit_time}");
    println!("cargo::rustc-env=GIT_HEAD={hash}");

    Ok(())
}
