# BLUG Show and See: preparation

The companion to [slides.md](slides.md): facts with their sources, the security claims and what
backs each one, the demo run sheet, and the questions to expect. Everything here was checked
against the code, the tests or hemlock on 14 November 2026; recheck anything that changes before
the talk.

To run the slides (Node 18 or newer):

```bash
cd docs/talks/2026-11-machinesdancing
npm install
npm run dev        # opens the deck; press "p" for presenter mode with the notes
npm run export     # slides.pdf, a fallback copy (needs playwright-chromium, installed on first run)
```

## Still to fill in

- The AI disclosure in your own words (slide 6, "How I work").
- Whether to show Claude Code live (slide 19). It works; keep the token file off screen.

## Fact sheet

| Question | Answer | Source |
|---|---|---|
| Host | HP EliteDesk 800 mini PC, i5-10500T (6 cores, 12 threads), 16 GB RAM, 238 GB NVMe | `docs/design/jete.md` (Host); `lscpu`, `free` |
| Host OS and kernel | Ubuntu Server 26.04.1 LTS, kernel 7.0.0-42-generic | `uname -r`, `/etc/os-release` |
| Hypervisor | Firecracker v1.17.0 on KVM, jailer v1.17.0 | `firecracker --version` |
| Guest kernel | 6.18.56, Firecracker CI's prebuilt build, pinned by checksum | `ansible/roles/guest_images` |
| Guest image | Minimal Ubuntu 26.04 by mmdebstrap, systemd, curl, Claude Code; 2 GB sparse ext4, about 520 MB used | `images/rootfs/build` |
| Disks | XFS reflinks: each VM's root filesystem is a copy-on-write clone | `crates/jete/src/vm.rs` (reflink) |
| Jail | Per VM: chroot, cgroup (memory capped at guest RAM plus 128 MiB), new PID namespace, own uid from 300000 | `crates/jete/src/vm.rs` (jailer arguments) |
| Firecracker config | A config file in the jail (`--config-file /vm.json`), plus an API socket | `crates/jete/src/vm.rs` |
| Guest control | vsock port 1024 to `jete-agent`, a 765 KB static Rust binary started by systemd after local filesystems | `crates/jete-agent`, `images/rootfs/overlay/etc/systemd/system/jete-agent.service` |
| Protocol | One session per connection; length-prefixed frames, JSON for control, raw for data, 1 MiB cap, exact version match | `crates/jete-protocol` |
| Ready | The agent's answer to a ping over vsock; jete retries until then, up to 30 s, then removes the VM | `crates/jete/src/vm.rs` (`wait_ready`, `READY_TIMEOUT`) |
| Time-to-ready | About 1.7 s (0.6 s kernel, 1.1 s userspace); same with a network | `jete ls`, `docs/design/jete.md` |
| VM network | Optional. A tap `jete-<id>` per VM, a /30 from 10.200.0.0/16 (host .1, guest .2), routed and masqueraded, IPv4 only | `crates/jete/src/net.rs` |
| Firewall | One nftables table, `inet jete`, nothing else touched | `crates/jete/src/net.rs` (`BASE_TABLE`, `PREROUTING`) |
| Egress, proxy mode | nftables `redirect` of all TCP and UDP 53 to barre's port on the VM's host side; original destination read with `SO_ORIGINAL_DST` | `crates/jete/src/net.rs`, `crates/barre/src/serve.rs` |
| DNS | barre answers it: allowed names get a synthetic `A` record from 198.18.0.0/15 (per VM), other types an empty answer, other names `NXDOMAIN` | `crates/barre/src/dns.rs`, `serve.rs` |
| Policy | Files in `/run/barre/policies/`, one per VM address, written by root with `barre allow`; picked up within 0.5 s | `crates/barre/src/policy.rs`, `serve.rs` |
| Rules | `NAME`, `*.NAME`, `ADDRESS[/PREFIX]`, then `:PORT,...` (443 by default) | `crates/barre/src/rules.rs` |
| barre's privileges | Its own user, no capabilities, systemd sandbox (read-only filesystem, IPv4 and Unix sockets only, system call filter) | `ansible/roles/barre/templates/barre.service.j2` |
| Logging | One JSON line per DNS answer and per connection, to the journal | `crates/barre/src/serve.rs` |
| Persistent storage | None yet: a VM's disk lives and dies with it | `docs/design/jete.md` |
| Size | About 5,000 lines of Rust in five crates; first commit 31 October 2026 | `wc -l crates/*/src/*.rs crates/*/tests/*.rs`, `git log` |

## Security claims

| Claim | Enforced by | Evidence | Limits |
|---|---|---|---|
| A guest can't reach the host's services | nftables input chain: only the VM's own host ports (or barre's) | `net.rs` test `a_networked_vm_reaches_only_what_it_is_given`: host SSH on the VM's link and on the LAN address time out | IPv4 tested; IPv6 dropped by rule, not tested |
| A guest can't reach the LAN or the tailnet | Direct mode: forward chain allows only listed addresses. Proxy mode: nothing forwarded; barre refuses non-public names and has only the address rules given | Same test (LAN gateway, 100.100.100.100); demo 2 (192.168.0.1 refused by barre) | An address rule naming the LAN would allow it: that's the caller's choice |
| VMs can't reach each other | Forward chains drop traffic to other taps, even through an allowed block | `net.rs` `vms_never_reach_each_other` | |
| Nothing connects into a VM, not even the host | Output and forward chains accept only replies toward a tap | `net.rs` (host to guest port 22 times out) | vsock is the host's way in, by design |
| DNS can't bypass the policy | barre answers all DNS; nothing is forwarded upstream for names not allowed | `crates/barre/tests/vm.rs` (github.io doesn't resolve); `net.rs` (DNS to any resolver reaches the proxy) | DNS over HTTPS to an allowed name would work, but only reaches that name |
| Direct-IP connections need an address rule | barre refuses an address it didn't hand out unless a rule covers it | barre `vm.rs` (8.8.8.8 refused, 1.1.1.1 allowed by rule); demo 2 | |
| A VM can't use another VM's synthetic address | Synthetic addresses are per VM | barre `vm.rs` `policies_stay_with_their_vm` | |
| A removed VM's policy doesn't pass to the next VM given its address | A policy records the interface name and index | barre `vm.rs` `policies_stay_with_their_vm` | |
| A guest can't spoof another source address | Per-VM chains drop anything not from the VM's own address | Rules in `crates/jete/src/net.rs`, checked by unit tests | **Not tested end to end** |
| IPv6, ICMP and other UDP don't leave | Each VM chain ends in `drop`; its rules match IPv4 only | Other UDP: `net.rs` proxy test. IPv6, ICMP: rules only | **IPv6 and ICMP not tested** |
| No egress if barre is down | jete only redirects to the port; nothing is forwarded | `net.rs` proxy test, "with nothing listening" | |
| Policy changes apply without touching the guest | barre re-reads policies every 0.5 s | barre `vm.rs` (revoke); demo 3 | Updated rules affect new connections only. Revoke closes open ones (code, **not tested**), and they aren't logged as closed |
| A guest process can't use the agent to become root | The agent accepts vsock connections only from the host (CID 2) | `crates/jete-agent/src/main.rs` | **Not tested** from inside a guest |
| Credentials aren't exposed to the guest | **Not true today**: Claude Code's token is in the VM while it runs | `scripts/claude-vm` | Phase 2: barre adds credentials |
| The VM can't escape to the host | Firecracker, KVM, the jailer | Upstream's design and testing | Not tested by this project; no claim beyond upstream's |

## Demo run sheet

On hemlock, in a terminal with a large font. Recorded runs are in [demos/](demos/).

| # | Action | Expected | Fallback |
|---|---|---|---|
| 0 | Before: `sudo jete ls`, `sudo barre ls`, `systemctl is-active barre` | No leftover demo VMs; barre `active` | `sudo barre-vm rm ID` for leftovers |
| 1 | `id=$(sudo jete run)` | `ready in ~1700 ms` | `demos/1-vm-lifecycle.txt` |
| 2 | `jete ls`, `exec uname -sr`, `exec ls /sys/class/net`, `exec ... exit 3` | `6.18.56+`, `lo`, `hi`, `exit 3` | same |
| 3 | `sudo jete rm $id` | Gone from `jete ls` | same |
| 4 | `id=$(sudo barre-vm run example.com github.com:22); sleep 1` | An id | `demos/2-egress-policy.txt` |
| 5 | Allowed: `getent hosts`, `curl -sI https://example.com`, SSH banner | `198.18.0.1`, `HTTP/2 200`, `SSH-2.0-...` | same |
| 6 | Refused: github.io, https://github.com, https://1.1.1.1, http://192.168.0.1 | Exit codes 6, 35, 35, 56 | same |
| 7 | The journal, filtered with `jq` | Four refusals with reasons, one `NXDOMAIN` | Backup slide "barre's log" |
| 7b | `sudo nft list table inet jete` | jete's table: the per-VM redirect, input and forward chains | Backup slide "a proxied µVM's nftables rules" |
| 8 | `barre allow ... 1.1.1.1; sleep 1`, then `curl -sI https://1.1.1.1` | `HTTP/2 301` | same |
| 9 | `barre revoke`, then curl example.com | `Could not resolve host` | same |
| 10 | `sudo barre-vm rm $id` | The id | |

Notes:

- **Don't skip the `sleep 1`** after `barre-vm run` and `barre allow`. barre picks up a policy
  within half a second; a request before that fails. (Seen while preparing: the first two
  requests failed.)
- **`jete exec` passes stdin through.** In a script, run it with `</dev/null`, or it reads the
  rest of the script.
- example.com, github.com and 1.1.1.1 are outside services. If the venue's network is down,
  demos 2 and 3 fail: use the transcripts.
- Never show the token file, `~/.config/machinesdancing/`, or `claude setup-token` output.

## Questions to expect

1. **Why not containers?** They share the host's kernel. For code nobody reviewed, I wanted a
   hypervisor boundary, and Firecracker makes a VM cheap: 1.7 s to ready, about 5 MB of memory
   overhead for Firecracker itself (upstream's figure).
2. **What does Firecracker give you, and what's yours?** Firecracker: the VM, the jailer. Mine:
   lifecycle, the guest agent and protocol, disks, the network setup and firewall, the gateway.
3. **Transparent or explicit proxy?** Transparent. nftables `redirect`, `SO_ORIGINAL_DST`. The
   guest has no proxy settings and can't opt out.
4. **How does DNS work?** barre answers every query itself, with synthetic addresses for allowed
   names. Nothing goes upstream for other names.
5. **Can the guest connect straight to an IP?** The connection reaches barre, which refuses it
   unless an address rule covers it.
6. **IPv6? UDP?** IPv6 is dropped (rules; not tested). UDP: only DNS, redirected to barre.
7. **Can the guest reach the host's tap address?** Only barre's port on it (or the host ports given,
   in direct mode). The host's SSH on that address times out (tested).
8. **What happens to open connections when you revoke?** They're closed. On a rule change, open
   ones carry on; new ones get the new rules.
9. **What can a compromised guest do?** Anything inside its VM; reach the names its policy allows;
   use credentials it holds (today, Claude Code's token). It can't reach other VMs, the host's
   services or the LAN, as far as tested.
10. **HTTPS: do you decrypt?** No. barre checks the server name in the handshake and passes the
    rest. Known gap: domain fronting. Phase 2 decrypts only where it adds credentials.
11. **What runs as root?** `jete` (the jailer needs it) and `barre allow`. Each Firecracker runs as
    its own user; barre's service as `barre`, without capabilities.
12. **How did you use AI, and how do you check it?** See slide 6, "How I work". Tests boot real VMs; every
    status claim points at one.

## Before presenting

1. **Decide on the Claude Code demo** (slide 19). If you show it, rehearse it like the others.
2. **Rehearse demos 1 to 3 twice** from a clean state, on the network you'll have there.
3. **Decide on the half-second gap**: keep the `sleep 1`, or make `barre allow` wait until barre
   is listening.
4. **Check slide 20's numbers** if more code lands: test counts, status labels.
5. **Export the PDF** and keep it with the transcripts, offline.
6. **Make the repository public**: the last slide links to it.
7. **The license is Apache 2.0** (PR #22). If asked why: permissive, so forks and company use
   are fine; it has a patent grant; and services can still be built on top.
