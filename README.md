# ❤️🪵

> ## Fork: Git LFS support, backed by IPFS
>
> This is a fork of upstream [`radicle-dev/heartwood`](https://github.com/radicle-dev/heartwood)
> adding Git LFS (large file) support, with large file content stored on each contributor's own
> local IPFS node rather than a central server. Everything below the horizontal rule is
> upstream's own README, unchanged. See [`LFS-IPFS.md`](LFS-IPFS.md) for the full design,
> troubleshooting, and background — this section is just the quick start.
>
> The LFS byte-transfer logic lives in a separate, small companion repository:
> [**radicle-lfs-transfer**](https://git.hswro.org/mab122/radicle-lfs-transfer), included here
> as a git submodule.
>
> ### Dependencies (Arch Linux)
>
> ```sh
> # To build and install rad/radicle-node/radicle-lfs-transfer
> sudo pacman -S --needed rust git openssh base-devel
>
> # To actually use Git LFS (not needed to build/install anything)
> sudo pacman -S --needed git-lfs kubo
> ```
>
> On other distributions: a Rust toolchain (e.g. via [rustup](https://rustup.rs)), Git, OpenSSH,
> a C toolchain — and, only for using `rad lfs`, [Git LFS](https://git-lfs.com) and
> [Kubo](https://docs.ipfs.tech/install/).
>
> ### Zero to usable
>
> ```sh
> # Clone with the submodule
> git clone --branch rad-lfs-ipfs --recurse-submodules \
>   ssh://git@git.hswro.org:9022/mab122/radicle-heartwood-lfs.git
> cd radicle-heartwood-lfs
>
> # Build & install rad, radicle-node, git-remote-rad, and rad-lfs-transfer to one place
> cargo install --path crates/radicle-cli --force --locked --root ~/.radicle
> cargo install --path crates/radicle-node --force --locked --root ~/.radicle
> cargo install --path crates/radicle-remote-helper --force --locked --root ~/.radicle
> cargo install --path radicle-lfs-transfer --force --locked --root ~/.radicle
>
> # Add the install root to your PATH (e.g. in ~/.bashrc / ~/.zshrc)
> export PATH="$HOME/.radicle/bin:$PATH"
>
> # Verify
> rad --version
> ```
>
> From here, use `rad` exactly as upstream describes below (`rad auth`, `rad init`, etc). The
> only new commands are `rad lfs init` (run once per repository you want large-file support in),
> `rad lfs rekey` (run after granting a new collaborator access to a **private** repository, so
> they can decrypt previously-committed LFS objects too), and `rad lfs backfill` (retroactively
> pins any LFS-tracked file that got committed without going through the pre-commit hook, e.g.
> `--no-verify`) — see [`LFS-IPFS.md`](LFS-IPFS.md) for those workflows and how private-repo
> content gets encrypted before it reaches IPFS.
> **Nothing above requires IPFS**; Git LFS support specifically needs a running `ipfs daemon`,
> and `rad lfs init` will tell you plainly if one isn't reachable rather than failing confusingly
> later.
>
> ### Quickstart cheatsheet
>
> One-time, per repository (with `ipfs daemon` already running in the background):
>
> ```sh
> rad lfs init
> git lfs track "*.psd"      # or whatever large-file patterns you need
> git add .gitattributes
> ```
>
> Day to day — same as plain Git LFS:
>
> ```sh
> git add my-large-file.psd
> git commit -m "Add asset"  # pre-commit hook pins it to IPFS, records the CID as a git note
> git push rad                # pushes your commit(s) and the refs/notes/rad-lfs mapping together
> ```
>
> Someone else, cloning the repository for the first time:
>
> ```sh
> rad clone rad:<repo-id>
> cd <repo>
> rad lfs init                # one-time, needs their own ipfs daemon running
> git lfs pull                 # fetches large files via IPFS instead of git
> ```
>
> **Private repository?** You'll just be prompted for your keystore passphrase on `commit`/
> `push`/`pull` when needed (ssh-agent alone can't do the key-agreement encryption requires —
> see [`LFS-IPFS.md`](LFS-IPFS.md#encryption-for-private-repositories)). After granting a new
> collaborator access, have an *already-authorized* collaborator run `rad lfs rekey` and push,
> so the newcomer can decrypt files committed before they were added:
>
> ```sh
> rad id update --allow <did>   # or add them as a delegate
> rad lfs rekey                 # run by someone already authorized
> git push rad                  # publish the updated wrapped keys
> ```
>
> **Repository set up with an older build?** A bare `git push rad` used to push *only* the
> notes mapping, silently leaving your commit unpushed (`rad lfs init` only configured a push
> refspec for the notes ref, not the branch). Fixed — but only for repositories where
> `rad lfs init` has been (re-)run with this build; re-run it once (harmless, idempotent) to
> pick up the fix on a repo set up before this. Until then, push both explicitly:
> `git push rad <branch>` then `git push rad`.
>
> Full design, encryption details, and a longer troubleshooting table:
> [`LFS-IPFS.md`](LFS-IPFS.md).

---

*Radicle Heartwood Protocol & Stack*

Heartwood is the third iteration of the Radicle Protocol, a powerful
peer-to-peer code collaboration and publishing stack. The repository contains a
full implementation of Heartwood, complete with a user-friendly command-line
interface (`rad`) and network daemon (`radicle-node`).

Radicle was designed to be a secure, decentralized and powerful alternative to
code forges such as GitHub and GitLab that preserves user sovereignty
and freedom.

See the [Radicle home page](https://radicle.dev/) for general
information, and the [Zulip chat](https://radicle.zulipchat.com/) to
talk to the project.

See the [Protocol Guide](https://radicle.dev/guides/protocol) for an
in-depth description of how Radicle works.

## Installation

**Requirements**

* *Linux* or *Unix* based operating system.
* Git 2.34 or later
* OpenSSH 9.1 or later with `ssh-agent`

### 📀 From binaries

> Requires `curl` and `tar`.

Run the following command to install the latest binary release:

    curl -sSf https://radicle.dev/install | sh

Or visit our [download](https://radicle.dev/download) page.

### 📦 From source

> Requires the Rust toolchain.

You can install the Radicle stack from source, by running the following
commands from inside this repository:

    cargo install --path crates/radicle-cli --force --locked --root ~/.radicle
    cargo install --path crates/radicle-node --force --locked --root ~/.radicle
    cargo install --path crates/radicle-remote-helper --force --locked --root ~/.radicle

Or directly from our seed node:

    cargo install --force --locked --root ~/.radicle \
        --git https://seed.radicle.dev/z3gqcJUoA1n9HaHKufZs5FCSGazv5.git \
        crates/radicle-cli crates/radicle-node crates/radicle-remote-helper

## Running

*Systemd* unit files are provided for the node under the `/systemd` folder.
They can be used as a starting point for further customization.

For running in debug mode, see [HACKING.md](HACKING.md).

## Feedback

If you have feedback, feel free to create issues using `rad issue`, join
[our Zulip][zulip], or email [feedback@radicle.dev][mail-feedback].
Emails sent to this address are [automatically posted][zulip-help-email] to
[our **public** #feedback channel on Zulip][zulip-feedback], revealing the
[`From` header][rfc2822s3.6.2] (which usually contains your name and email
address). This allows us to discuss your feedback on Zulip, and, if necessary,
respond to you via email.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [HACKING.md](HACKING.md) for an
introduction to contributing to Radicle.

## License

Radicle is distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.

[zulip]: https://radicle.zulipchat.com/
[zulip-feedback]: https://radicle.zulipchat.com/#narrow/channel/392584-feedback
[zulip-help-email]: https://talently.zulip.com/help/message-a-channel-by-email
[mail-feedback]: mailto:feedback@radicle.dev
[rfc2822s3.6.2]: https://datatracker.ietf.org/doc/html/rfc2822#section-3.6.2
