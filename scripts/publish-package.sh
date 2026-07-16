#!/usr/bin/env bash
#
# Builds release binaries for the LFS-over-IPFS stack (rad, radicle-node,
# git-remote-rad, rad-lfs-transfer) and publishes them as a Forgejo generic
# package, so a deploy target can `curl` a tarball instead of needing a full
# Rust toolchain (see NOTES-lfs-store-note-write-bug.md's VPS deployment
# story for why that matters -- rebuilding in place isn't always possible).
#
# Usage:
#   FORGEJO_TOKEN=... ./scripts/publish-package.sh [version]
#
# `version` defaults to `git describe --tags --always --dirty`, sanitized
# for use in a URL path. The token needs the `write:package` scope; it's
# read from $FORGEJO_TOKEN, falling back to ~/.config/forgejo/token if that
# env var isn't set (chmod 600 that file if you use it).
#
# Config (override via env):
#   FORGEJO_HOST    default: git.hswro.org
#   FORGEJO_OWNER   default: mab122
#   PACKAGE_NAME    default: radicle-lfs-ipfs

set -euo pipefail

FORGEJO_HOST="${FORGEJO_HOST:-git.hswro.org}"
FORGEJO_OWNER="${FORGEJO_OWNER:-mab122}"
PACKAGE_NAME="${PACKAGE_NAME:-radicle-lfs-ipfs}"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if [ -n "${FORGEJO_TOKEN:-}" ]; then
    token="$FORGEJO_TOKEN"
elif [ -r "$HOME/.config/forgejo/token" ]; then
    token="$(cat "$HOME/.config/forgejo/token")"
else
    echo "error: no Forgejo API token found. Set \$FORGEJO_TOKEN, or put one" \
         "(scope: write:package) in ~/.config/forgejo/token (chmod 600)." >&2
    exit 1
fi

version_raw="${1:-$(git describe --tags --always --dirty)}"
# Forgejo generic package versions must be URL-path-safe; a `git describe`
# with a "-dirty" suffix or a leading "v" is otherwise fine, but sanitize
# defensively rather than let a stray character break the upload URL.
version="$(printf '%s' "$version_raw" | tr -c 'A-Za-z0-9._-' '-')"

if [ ! -f "$repo_root/radicle-lfs-transfer/Cargo.toml" ]; then
    echo "error: radicle-lfs-transfer/ submodule is empty -- run" \
         "'git submodule update --init' first." >&2
    exit 1
fi

echo "==> Building release binaries (version: $version)"
cargo build --release --locked -p radicle-cli --bin rad
cargo build --release --locked -p radicle-node
cargo build --release --locked -p radicle-remote-helper
cargo build --release --locked --manifest-path radicle-lfs-transfer/Cargo.toml

target_triple="$(rustc -vV | sed -n 's/host: //p')"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

cp target/release/rad "$staging/"
cp target/release/radicle-node "$staging/"
cp target/release/git-remote-rad "$staging/"
cp radicle-lfs-transfer/target/release/rad-lfs-transfer "$staging/"

# Recorded because a binary built on a newer glibc can fail to run on an
# older one (discovered live deploying this exact stack to a Debian VPS) --
# anyone consuming this package needs to be able to check compatibility
# before copying the binaries over, not find out by them failing to start.
cat > "$staging/MANIFEST.txt" << EOF
package:        $PACKAGE_NAME
version:        $version
git commit:     $(git rev-parse HEAD)
git branch:     $(git rev-parse --abbrev-ref HEAD)
built:          $(date -u +%Y-%m-%dT%H:%M:%SZ)
target triple:  $target_triple
rustc:          $(rustc --version)
glibc:          $(ldd --version | head -1)
contents:
  rad                 $(target/release/rad --version)
  radicle-node        $(target/release/radicle-node --version)
  git-remote-rad       $(target/release/git-remote-rad --version)
  rad-lfs-transfer     (no --version; speaks git-lfs custom-transfer protocol on stdin/stdout)
EOF

archive_name="${PACKAGE_NAME}-${version}-${target_triple}.tar.gz"
archive_path="$staging/../${archive_name}"
tar -C "$staging" -czf "$archive_path" .
sha256sum "$archive_path" | awk '{print $1}' > "${archive_path}.sha256"

echo "==> Built $(du -h "$archive_path" | cut -f1) archive: $archive_name"

upload() {
    local file="$1" name="$2"
    local url="https://${FORGEJO_HOST}/api/packages/${FORGEJO_OWNER}/generic/${PACKAGE_NAME}/${version}/${name}"
    local status
    status="$(curl -sS -o /tmp/forgejo-upload-response.$$ -w '%{http_code}' \
        -H "Authorization: token ${token}" \
        -X PUT "$url" \
        -T "$file")"
    if [ "$status" = "201" ]; then
        echo "    uploaded: $name"
    elif [ "$status" = "409" ]; then
        echo "    error: $name already exists at version $version -- bump the version" \
             "or delete the existing package file in Forgejo first." >&2
        cat /tmp/forgejo-upload-response.$$ >&2
        rm -f /tmp/forgejo-upload-response.$$
        exit 1
    else
        echo "    error: upload of $name failed (HTTP $status)" >&2
        cat /tmp/forgejo-upload-response.$$ >&2
        rm -f /tmp/forgejo-upload-response.$$
        exit 1
    fi
    rm -f /tmp/forgejo-upload-response.$$
}

echo "==> Uploading to https://${FORGEJO_HOST}/${FORGEJO_OWNER}/-/packages/generic/${PACKAGE_NAME}/${version}"
upload "$archive_path" "$archive_name"
upload "${archive_path}.sha256" "${archive_name}.sha256"

echo "==> Done: https://${FORGEJO_HOST}/${FORGEJO_OWNER}/-/packages/generic/${PACKAGE_NAME}/${version}"
