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

## Try it

From a fresh clone, on the Mac. The guides call the Linux host `hemlock`, so the commands below do
too.

1. **Set up the Mac and the host.** Install the tools as in
   [development setup](docs/guides/development.md#mac-setup), then take over the host (an Ubuntu
   Server install you can reach with `ssh-copy-id`, added to `ansible/inventory.yml`) and
   configure it:

   ```bash
   cd ansible
   ansible-galaxy collection install -r requirements.yml
   ansible-playbook site.yml
   cd ..
   ```

   See [ansible/README.md](ansible/README.md#taking-over-a-new-host).

2. **Build the guest image and install jete.** The image is built on the host; jete is built on
   the Mac and copied over:

   ```bash
   scripts/build-rootfs
   scripts/install-jete
   ```

3. **Boot a µVM and run a command in it:**

   ```bash
   id=$(ssh hemlock sudo jete run)
   ssh hemlock sudo jete exec $id -- uname -a
   ssh hemlock sudo jete rm $id
   ```

   More in [running jete](docs/guides/development.md#running-jete).

4. **Run Claude Code in a µVM behind barre.** Install barre, save a token for your Claude
   subscription on the Mac (`claude setup-token`, then
   `~/.config/machinesdancing/claude-token`), and run a prompt:

   ```bash
   scripts/install-barre
   scripts/claude-vm "what is in your home directory?"
   ```

   The µVM may reach `api.anthropic.com` and nothing else. The token steps, letting it reach
   more, and cleaning up are in [claude-code.md](docs/guides/claude-code.md).

## Security

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

[Apache License 2.0](LICENSE). Copyright 2026 Carlos Westman.
