# ❤️🪵

> ## Fork: Git LFS support, backed by IPFS
>
> This is a fork of upstream [`radicle-dev/heartwood`](https://github.com/radicle-dev/heartwood)
> adding Git LFS (large file) support, with large file content stored on each contributor's own
> local IPFS node rather than a central server. Everything below is upstream's own text,
> unchanged, and describes upstream `heartwood` in general, not this fork specifically.
>
> This repository is developed on a self-hosted Forgejo instance, not GitHub — see
> [the main `README.md`](../README.md) for the fork's own quickstart, and
> [`LFS-IPFS.md`](../LFS-IPFS.md) for the full design, encryption details, and troubleshooting.

*Radicle Heartwood Protocol & Stack*

Heartwood is the third iteration of the Radicle Protocol, a powerful
peer-to-peer code collaboration and publishing stack. The repository contains a
full implementation of Heartwood, complete with a user-friendly command-line
interface (`rad`) and network daemon (`radicle-node`).

[Radicle][homepage] is a secure, decentralized and powerful alternative to
centralized code forges such as GitHub and GitLab that preserves user
sovereignty and freedom.

See the [Protocol Guide][guides-protocol] for an in-depth description of how
Radicle works.

For more information about [installing][install] and [running][run] Radicle, how
to contribute, and licensing, please refer to the [main `README.md` of the
repository][readme].

## Radicle on GitHub

Please note that GitHub is not the forge that Radicle is being developed with.
Radicle is developed using Radicle. Therefore, issues cannot be created on
GitHub, but only via Radicle directly. However, pull requests are still
accepted.

To view existing issues and patches with your web browser, navigate to
[radicle.network][heartwood].

To contribute issues and patches, please consider [installing][install]
Radicle and follow our [guides][guides].

## Feedback

If you have feedback, feel free to create issues using `rad issue`, join
[our Zulip][zulip], or email [feedback@radicle.dev][mail-feedback].
Emails sent to this address are [automatically posted][zulip-help-email] to
[our **public** #feedback channel on Zulip][zulip-feedback], revealing the
[`From` header][rfc2822s3.6.2] (which usually contains your name and email
address). This allows us to discuss your feedback on Zulip, and, if necessary,
respond to you via email.


[guides-protocol]: https://radicle.dev/guides/protocol
[guides]: https://radicle.dev/guides
[heartwood]: https://radicle.network/nodes/seed.radicle.dev/rad:z3gqcJUoA1n9HaHKufZs5FCSGazv5
[homepage]: https://radicle.dev
[install]: ../README.md#installation
[mail-feedback]: mailto:feedback@radicle.dev
[readme]: ../README.md
[rfc2822s3.6.2]: https://datatracker.ietf.org/doc/html/rfc2822#section-3.6.2
[run]: ../README.md#running
[zulip-feedback]: https://radicle.zulipchat.com/#narrow/channel/392584-feedback
[zulip-help-email]: https://talently.zulip.com/help/message-a-channel-by-email
[zulip]: https://radicle.zulipchat.com/
