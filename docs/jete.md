# jete — design

jete (from *jeté*, a ballet leap) is the first tool: a VM manager. It creates, runs and destroys
lightweight VMs fast, on one Linux host with KVM. It is general-purpose: it knows nothing about
agents, roles, messages or GitHub. Higher-level tools build on its interface.

## Scope

**Does**

- Manage images: a guest kernel plus a read-only root filesystem.
- Run the VM lifecycle: create, start, stop, destroy, list, inspect.
- Give each VM a copy-on-write disk on top of a shared base image.
- Attach extra read-only disks the caller provides (for example, shared files such as skills).
- Expose each VM's vsock to the caller as a host-side Unix socket. It carries bytes and does not
  interpret them.
- Apply the network setting the caller asks for: none (the default), a tap device that reaches
  only the addresses and host ports the caller allows, or one whose traffic all goes to a proxy
  port on the host (the egress gateway's).
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
| M1 ✓ | One VM | `jete` boots a jailed VM from a kernel and root filesystem, and the host runs a command and opens a shell in it over vsock. |
| M2 ✓ | Network | `jete run --allow <address>` gives a VM a network link that reaches only the addresses and host ports it is given, and nothing else: not the host, the LAN, the tailnet or other VMs. Teardown leaves nothing behind. |
| M3 | Daemon and API | The daemon owns VM lifecycles; the CLI becomes its client. |
| M4 | Speed | Snapshot and restore, plus a warm pool. |

M1 also delivered most of what was first planned as M2, "Many VMs": copy-on-write disks, many VMs
at once, `ls` and `rm` with no leftovers. Stopping a VM while keeping its disk, and a disk size per
VM, are left for when something needs them.

From M1 on, every milestone reports **time-to-ready**: how long from the create request until the
guest answers on vsock.

## M1 plan

| Concern | Decision |
|---|---|
| Guest kernel | Firecracker CI's prebuilt 6.18 guest kernel (the newest LTS line they build), pinned by CI build and checksum and installed by the `guest_images` Ansible role as `/var/lib/vms/images/kernels/vmlinux`. It has virtio block, vsock, ext4, cgroups and namespaces built in, and supports modules. Building our own from their config comes later, when we need something it leaves out. To find a newer build: list `https://s3.amazonaws.com/spec.ccfc.min/?prefix=firecracker-ci/&delimiter=/&list-type=2`, take the newest dated prefix, and list `<prefix>x86_64/vmlinux-` in it. |
| Root filesystem | Minimal Ubuntu 26.04 (`mmdebstrap --variant=minbase` plus systemd, udev and CA certificates, about 95 packages) as a 2 GB sparse ext4 image, about 230 MB on disk. Built on hemlock by [`images/rootfs/build`](../images/rootfs/build), run from the Mac with `scripts/build-rootfs`, in about 30 seconds. Each build is a new, read-only, timestamped file in `/var/lib/vms/images/rootfs/` with its package manifest beside it; `ubuntu-26.04.ext4` points at the newest. systemd is the init: agents get a normal Ubuntu. No network configuration (M1 VMs have no NIC), root's password is locked, and the machine id is generated at first boot. It boots in 1.7 s under Firecracker (0.6 s kernel, 1.1 s userspace), the baseline M4 improves on. |
| Guest agent | `jete-agent`: a static (musl) Rust binary, 765 KB, baked into the root filesystem and started by systemd as soon as local filesystems are mounted. It listens on vsock port 1024 and answers a ping (the time-to-ready signal) and runs processes, with pipes or a pseudo-terminal (which is what a shell is), streaming stdin, stdout and stderr and returning the exit status. It knows nothing about agents or messages; higher layers use other vsock ports. Measured time-to-ready: about 1.7 s from starting Firecracker, of which systemd's own startup takes about 0.9 s before it starts anything. |
| Protocol | `crates/jete-protocol`, shared by both sides. One connection is one session: a request, then length-prefixed frames (raw bytes for data, JSON for control). Strict by design: an exact version match, unknown fields and frame kinds rejected, frames capped at 1 MiB, and only HUP, INT, QUIT, TERM and KILL accepted as signals. The agent accepts connections only from the host (vsock CID 2), so processes in the guest can't use it to become root. Each process starts from a minimal environment and leads its own process group; if the host disconnects, the whole group is killed. |
| VM layout | Each VM gets a random 8-hex-digit id and its own user id (from 300000, allocated under a lock). Its jail, `/var/lib/vms/jailer/<firecracker binary>/<id>/root`, holds a hard link to the kernel, a reflinked copy of the root filesystem (writable only by the VM's user), the Firecracker config, and the API and vsock sockets. The jailer adds a cgroup, `/sys/fs/cgroup/jete/<id>`, capping memory at guest RAM plus 128 MiB, a new PID namespace and a `pivot_root` into the jail. jete's record (`vm.json`, including the resolved image paths and time-to-ready) and the console log live in `/var/lib/vms/state/<id>`. `jete rm` kills the process through a pidfd (so a reused pid is never hit), waits for it to exit, and deletes the cgroup, jail and state. |
| Privileges | `jete` runs under `sudo` in M1, because the jailer starts as root (chroot, device nodes, cgroups) before dropping each VM to an unprivileged user. The admin user has passwordless `sudo` on hemlock (the `access` role), so tests that boot VMs run unattended. The daemon and its user model come in M3. |
| Crates | One Cargo workspace: `crates/jete` (library, including the host side of the agent channel), `crates/jete-cli` (the `jete` binary), `crates/jete-agent` (guest agent), `crates/jete-protocol` (the wire protocol between them). The toolchain and the Linux target are pinned in `rust-toolchain.toml`. |
| Build and test | Build on the Mac, test on hemlock. The Mac (Apple M4) compiles much faster, and hemlock carries no compilers. Everything targets `x86_64-unknown-linux-musl` by default (set in `.cargo/config.toml`), producing static Linux binaries. A cargo *runner* copies each test binary to hemlock and runs it there, because booting VMs needs KVM. See [development.md](development.md). `scripts/install-jete` installs a release build of `jete` on hemlock through Ansible (`ansible/jete.yml`); release builds in CI, pinned by version, come later. |

**M1 is done when**, on hemlock (see [development.md](development.md#running-jete)):

```bash
sudo jete run --kernel <vmlinux> --rootfs <image>   # boots a jailed VM, prints its id and time-to-ready
sudo jete exec <id> -- uname -a                     # runs a command over vsock
sudo jete shell <id>                                # interactive shell over vsock
sudo jete rm <id>                                   # stops it and removes its jail
```

Done: time-to-ready is about 1.7 s, and `jete ls` lists VMs too.

## M2 plan

jete's part of networking, as set out in [architecture.md](architecture.md): a network link that
reaches only the addresses it is given. Allowing by name, logging and credentials belong to the
egress gateway, barre, a separate tool built next. To jete it is just a port that a VM's traffic
is handed to ([Egress proxy](#egress-proxy)).

| Concern | Decision |
|---|---|
| Default | No network, as in M1: no network device at all. A VM gets one only when `jete run` is given `--allow`, `--host-port` or `--egress-proxy` (see [Egress proxy](#egress-proxy)). Deny by default (principle 4): a VM with a network reaches exactly what it was given. |
| Options | `--allow <address or CIDR>`: an IPv4 destination the VM may reach, any port and protocol (`1.1.1.1`, `140.82.112.0/20`); repeatable. `--host-port <port>`: a TCP or UDP port on the host side of the VM's link that the VM may reach (`53`, `443`); repeatable. `--dns <address>`: the resolver the guest uses; it must be reachable through one of the other two. |
| Addressing | IPv4 only. Each VM gets a /30 from `10.200.0.0/16` (16,384 VMs), allocated under the same lock as its user id and recorded in `vm.json`: the host side is `.1`, the guest `.2`. The guest kernel configures `eth0` from the boot line (`ip=…`), so the guest runs no network daemon and time-to-ready should not grow. `/etc/resolv.conf` in the image points at `/proc/net/pnp`, where the kernel writes the resolver from `--dns`. |
| Host side | A tap device per VM, `jete-<id>`, owned by the VM's user so the jailed Firecracker can open it, in the host's network namespace. Traffic is routed (not bridged) and masqueraded out of the host's default route. |
| Firewall | jete owns one nftables table, `inet jete`, and touches nothing else (Tailscale's tables stay as they are). Per VM, rules let it start connections only to its allowed addresses and to its host ports on `.1`; replies come back. Everything else is dropped: the host's other addresses and ports, and the LAN and the tailnet unless an allowed address covers them. Other VMs are never reachable, even through an allowed address. Nothing can open a connection into a VM, not even the host. IPv6 from a VM is dropped. Traffic that doesn't involve a VM is not affected. |
| Teardown | `jete rm` also deletes the VM's tap and its rules. A test checks that neither is left. |
| `jete ls` | Shows each VM's address, or `-` without a network. |
| Host setup | A `network` Ansible role: turns on IPv4 forwarding and installs `nftables`, keeping its boot-time service off (it would flush every table, Tailscale's included). It also brings up the host's wired ports with DHCP, preferred over Wi-Fi. |
| Tests | On hemlock: an allowed address is reachable and one not allowed isn't; a host port given is reachable and the host's SSH port isn't; the LAN and another networked VM aren't reachable; DNS works through an allowed resolver; a VM without network options has no network device; nothing is left after `rm`. |

**M2 is done when**, on hemlock:

```bash
id=$(sudo jete run --allow 1.1.1.1 --dns 1.1.1.1)
sudo jete exec $id -- bash -c 'echo > /dev/tcp/1.1.1.1/443'          # connects
sudo jete exec $id -- getent hosts github.com                         # resolves through 1.1.1.1
sudo jete exec $id -- bash -c 'echo > /dev/tcp/8.8.8.8/443'          # fails: not allowed
sudo jete exec $id -- bash -c 'echo > /dev/tcp/192.168.0.1/80'       # fails: the LAN
sudo jete rm $id                                                       # no tap or rules left
```

and time-to-ready with a network is reported next to M1's 1.7 s.

Done: time-to-ready with a network is 1.7–1.8 s, the same as without one. The tests in
`crates/jete/tests/net.rs` check each of the above, plus the host's SSH, the tailnet and other VMs.

### Egress proxy

Added for the egress gateway, barre ([barre.md](barre.md#who-owns-what)): a second way out,
instead of the allowlist, never with it.

| Concern | Decision |
|---|---|
| Option | `--egress-proxy <port>`. jete refuses it with `--allow`, `--host-port` or `--dns`, so a VM's egress has one owner. |
| Firewall | Each VM's TCP to any address and port, and its UDP to port 53, are redirected to `<port>` on its own `.1` (nftables `redirect`, in a per-VM chain reached from a `prerouting` NAT chain). The proxy reads each connection's original destination with `SO_ORIGINAL_DST`. The input chain lets in only that port; nothing is forwarded. Other UDP, ICMP and IPv6 are dropped. |
| Resolver | The guest's resolver is `.1`, though DNS sent anywhere ends up at the proxy. |
| Fails closed | jete doesn't know what listens on the port. If nothing does, the VM reaches nothing. |
| Older tables | The `prerouting` chain is added to a table created by an older jete, which lasts until the host reboots. |

The tests use a stand-in listener: connections to the internet, a synthetic address, another host
port and the LAN all arrive at it with their original destination; DNS to any resolver arrives;
other UDP doesn't; with no listener nothing gets through; and `--egress-proxy` with `--allow` is
refused.

## Open questions

1. Remote access to the daemon: over SSH or Tailscale only, or a network API with authentication?
