# machinesdancing

A harness for running a team of AI agents as a governed system, built from small, independent
tools.

> **Status: experimental.** A work in progress, built in the open; not for production. What
> works today and what comes next is tracked in [docs/build-order.md](docs/build-order.md). Built
> and tested so far: jete (Firecracker µVMs), barre (egress by name), and Claude Code running in a
> µVM behind barre.

## What you need

- **A Linux host with KVM**, x86_64 (`/dev/kvm`). Tested on Ubuntu Server 26.04, configured
  entirely by the playbooks in [`ansible/`](ansible/): Firecracker and its jailer, nftables, and
  XFS with reflinks for µVM disks.
- **A machine to build on** with [rustup](https://rustup.rs). The repo pins the toolchain and
  builds static `x86_64-unknown-linux-musl` binaries; tests run on the host over SSH. Tested from
  macOS on Apple Silicon. See [docs/guides/development.md](docs/guides/development.md).

| Start here | |
|---|---|
| [PRINCIPLES.md](PRINCIPLES.md) | Vision and principles. Every design decision must comply with them. |
| [docs/architecture.md](docs/architecture.md) | The big picture: the tools, how they connect, where things are stored. |
| [docs/build-order.md](docs/build-order.md) | What we build next, in what order, and where each step stands. |
| [docs/design/jete.md](docs/design/jete.md) | Design of jete, the first tool: a fast Firecracker VM manager. |
| [docs/design/barre.md](docs/design/barre.md) | Design of barre, the egress gateway: what VMs may reach, by name. |
| [docs/design/instances.md](docs/design/instances.md) | Design of instances: one team's setup, kept outside this repo. |
| [docs/design/repo-access.md](docs/design/repo-access.md) | Design of repo access: agents use git as developers, as a GitHub App per role. |
| [docs/guides/development.md](docs/guides/development.md) | Setting up the Mac, and how building and testing work. |
| [docs/guides/claude-code.md](docs/guides/claude-code.md) | Running Claude Code in a VM behind barre, on your Claude subscription. |
| [ansible/](ansible/) | Configuration of the hosts that run everything (currently hemlock). |

## Security

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

[Apache License 2.0](LICENSE). Copyright 2026 Carlos Westman.
