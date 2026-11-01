# jete — design

jete (from *jeté*, a ballet leap) is the first tool: a VM manager. It creates, runs and destroys
lightweight VMs fast, on one Linux host with KVM. It is general-purpose: it knows nothing about agents, roles, messages or GitHub. Higher-level
tools build on its interface.

## Scope

**Does**

- Manage images: a guest kernel plus a read-only root filesystem.
- Run the VM lifecycle: create, start, stop, destroy, list, inspect.
- Give each VM a copy-on-write disk on top of a shared base image.
- Expose each VM's vsock to the caller as a host-side Unix socket. It carries bytes and does not
  interpret them.
- Apply the network setting the caller asks for: none (the default), or a tap device with an
  egress allowlist.
- Set resource limits per VM: vCPUs, memory, disk size.
- Snapshot and restore VMs, and keep a warm pool of pre-booted VMs for near-instant starts.

**Does not**

- Know what runs inside a VM, or why.
- Interpret vsock traffic. Schemas and rules belong to a layer above.
- Schedule across hosts, or integrate with Kubernetes.
- Provision the host. Ansible does that.

## Stack

| Concern | Choice | Why |
|---|---|---|
| Hypervisor | Firecracker on KVM | Boots in milliseconds, small attack surface, written in Rust. |
| Process isolation | Firecracker jailer | Wraps each VM process in a chroot, cgroups, seccomp filters and its own user. |
| Language | Rust (tokio, axum) | Same language as Firecracker; its API is plain HTTP over a Unix socket, so no SDK is needed. |
| Disks | XFS with reflinks at `/var/lib/vms`: a per-VM disk is a `cp --reflink` of its base image | Instant, no block copy. Plain files fit the jailer's chroot, snapshots share the same filesystem, and a full disk fails cleanly instead of corrupting like an overcommitted thin pool. |
| Host-guest channel | vsock | Needs no network. The only path in or out unless networking is enabled. |
| Host provisioning | Ansible, run from the Mac over SSH | Declarative and idempotent. Ansible never creates or destroys VMs. |

## Host

hemlock: HP EliteDesk 800, i5-10500T (6 cores, 12 threads), 16 GB RAM, 238 GB NVMe, Ubuntu
Server 26.04, provisioned by `ansible/`. After the host's own needs, roughly 12 GB of RAM is left
for VMs, which sets how many can run at once.

## Interfaces

Three layers. Each is usable on its own:

1. **Library crate.** The core: images, VMs, disks, networking, snapshots.
2. **Daemon.** A host service that owns VM state and serves an API over a local Unix socket
   (remote access to be decided).
3. **CLI.** A thin client of the daemon, with machine-readable output (`--json`) so other tools
   can script it.

## Milestones

| | Goal | Done when |
|---|---|---|
| M0 ✓ | Host bootstrap | An Ansible playbook takes a fresh Ubuntu Server to "can run jailed Firecracker." |
| M1 | One VM | `jete` boots a jailed VM from a kernel and root filesystem, and the host runs a command and opens a shell in it over vsock. |
| M2 | Many VMs | Copy-on-write disks, optional tap device and egress allowlist, list, stop and destroy, with no leftovers after teardown. |
| M3 | Daemon and API | The daemon owns VM lifecycles; the CLI becomes its client. |
| M4 | Speed | Snapshot and restore, plus a warm pool. |

From M1 on, every milestone reports **time-to-ready**: how long from the create request until the
guest answers on vsock.

## M1 plan

| Concern | Decision |
|---|---|
| Guest kernel | Firecracker CI's prebuilt guest kernel, to boot quickly. Building our own from their recommended config comes later, when we need features it leaves out. Stored under `/var/lib/vms/images/`. |
| Root filesystem | A minimal Ubuntu 26.04 image as an ext4 file under `/var/lib/vms/images/`, built on hemlock by a script in this repo. |
| Guest agent | `jete-agent`: a small static (musl) Rust binary started at boot. It listens on a fixed vsock port and does three things: answers when the guest is ready (the time-to-ready signal), runs a command and returns stdout, stderr and the exit code, and opens an interactive shell. It knows nothing about agents or messages; higher layers use other vsock ports. |
| Privileges | `jete` runs under `sudo` in M1, because the jailer starts as root (chroot, device nodes, cgroups) before dropping each VM to an unprivileged user. The daemon and its user model come in M3. |
| Crates | One Cargo workspace: `crates/jete` (library), `crates/jete-cli` (the `jete` binary), `crates/jete-agent` (guest agent). The toolchain is pinned in `rust-toolchain.toml`, matching the `rust` Ansible role. |
| Build and test | On hemlock, natively: it is the target platform and the only machine with KVM. Code is edited in the repo, synced to a working copy on hemlock, and built and tested there over SSH. hemlock needs no GitHub credentials. Release builds in CI with pinned deployment by Ansible come later. |

**M1 is done when**, on hemlock:

```bash
sudo jete run --kernel <vmlinux> --rootfs <image>   # boots a jailed VM, prints its id and time-to-ready
sudo jete exec <id> -- uname -a                     # runs a command over vsock
sudo jete shell <id>                                # interactive shell over vsock
sudo jete rm <id>                                   # stops it and removes its jail
```

## Open questions

1. Remote access to the daemon: over SSH or Tailscale only, or a network API with authentication?
