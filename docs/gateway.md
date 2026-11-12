# barre — design

barre is the egress gateway: it carries every connection a VM makes to the outside world. It
allows each connection by the **name** the VM asked for, checked against that VM's allowlist; logs
every request; and, in a later phase, adds credentials so agents never hold them. Its place in the
whole is in [architecture.md](architecture.md); the network it sits on is jete's M2
([jete.md](jete.md#m2-plan)).

The name is from ballet, like jete's: the barre is the rail dancers hold on to, something to lean
on and a barrier.

## Scope

**Does**

- Receive everything a VM sends out, whatever the protocol, and decide it: allow, or refuse.
- Answer the VM's DNS: names on its allowlist resolve, everything else doesn't.
- Pass allowed connections through to the name they were made for, on the ports the allowlist
  gives that name.
- Log every DNS answer and connection, allowed or not.
- Later: add credentials to requests for configured hosts.

**Does not**

- Know about agents, roles or messages. It sees policies per address: "this address may reach these
  names, on these ports".
- Create VMs or networks: jete does. barre doesn't read jete's state either; it's told what to
  allow.
- Decrypt traffic in the first phase. Rules apply to whole hosts and ports, not URL paths.

## Who owns what

A VM's network has two concerns, and each has exactly one owner:

| | Owner | What it guarantees |
|---|---|---|
| **Isolation** | jete | The VM's traffic leaves only through the one exit the caller chose. It can't use another address than its own, reach the host's other services, the LAN, the tailnet or other VMs, or receive connections. |
| **Egress policy** | the exit | What the VM may reach beyond that, and the record of it. |

jete offers two exits, and a VM has exactly one of them:

- **Direct** (`--allow`, `--host-port`, `--dns`, as built in M2): jete's own address allowlist is
  the policy. For development, tests, and VMs that need no names.
- **Proxy** (`--egress-proxy <port>`, new): jete hands **all** of the VM's traffic to one port on
  the host and drops everything else. Whatever listens there is the policy. jete doesn't know it's
  barre.

`--egress-proxy` can't be combined with the direct options; jete refuses. So there's no VM whose
egress is split between jete's rules and barre's: either jete decides everything, or barre does.

This is the same split as a service mesh, where the firewall sends all of a workload's traffic to
its proxy and the proxy holds the policy (Istio and Envoy). It's also why the guest needs no proxy
settings: the redirection happens on the host, outside the VM's reach (principle 1).

### The contract between them

Three things, and nothing else. Neither tool reads the other's state.

1. **The port.** The caller passes barre's port to `jete run --egress-proxy`. If nothing listens
   there, the VM reaches nothing: it fails closed.
2. **Identity.** A VM's traffic arrives at the VM's own host-side address (`.1`), from its guest
   address (`.2`). jete guarantees both: the address is the VM's alone, and its firewall drops
   anything a VM sends from another source. barre identifies the VM by them, never by anything the
   VM says.
3. **Lifecycle.** When jete removes a VM, its tap and address disappear. barre drops the address's
   policy as soon as it sees that, so a VM removed without `barre revoke` can't pass its policy on to
   the next VM given the address.

## What jete does with `--egress-proxy`

| Traffic from the VM | What happens |
|---|---|
| TCP, to any address and port | Redirected to `<port>` on the VM's `.1` (an nftables `redirect`). The kernel keeps the original destination, which barre reads from the connection (`SO_ORIGINAL_DST`). |
| UDP to port 53 (DNS) | Redirected to `<port>` on `.1`, over UDP. |
| Everything else: other UDP (QUIC falls back to TCP), ICMP, IPv6 | Dropped. |
| Forwarded to anywhere | Nothing: the VM's traffic never leaves the host except as barre's own connections. |

The guest's resolver is set to `.1`. It hardly matters: DNS sent anywhere ends up at barre.

## How barre decides

### Names become addresses the VM can't misuse

barre answers DNS for an allowed name with a **synthetic address** from `198.18.0.0/15` (a range
reserved for benchmarking, never used on the internet), picked for that VM and that name. barre
remembers which name each address stands for. When a connection arrives, its original destination
tells barre which name it was made for, whatever the protocol: HTTPS, git over SSH, or anything
else on TCP.

Then barre:

1. checks the name and the port against the VM's allowlist;
2. resolves the name itself, through the host's resolver, and refuses non-public results;
3. connects there, and copies bytes both ways without reading them.

Synthetic addresses are per VM, so one VM can't use another's. Code that skips DNS and connects to a
real address gets a refusal, unless an address rule allows it.

### Policies

One per VM, keyed by its host-side address:

```bash
sudo barre allow 10.200.0.5 --vm 3f2a9c1b \
    api.anthropic.com '*.githubusercontent.com' github.com:22,443 140.82.112.0/20:22
sudo barre revoke 10.200.0.5
```

| Rule | Allows |
|---|---|
| `github.com` | That exact name, port 443. |
| `*.githubusercontent.com` | Its subdomains, not the bare name, port 443. |
| `github.com:22,443` | That name, on those ports. |
| `140.82.112.0/20:22` | That address block, on those ports, for clients that don't use DNS. The only way to reach an address directly. |

Names match case-insensitively, without a trailing dot.

### Checks per protocol

On top of the name and port, barre checks the protocols it can read:

| Port | Extra check |
|---|---|
| 443 | The name in the TLS handshake (SNI) must be the name the address stands for. No SNI, or another name: refused. The handshake is passed on unchanged. |
| 80 | Plain HTTP only if the rule names port 80, since anyone on the path can change it. The `Host` header must be the name. |
| Others | None: the name and port are the whole rule. |

## Decisions

| Concern | Decision |
|---|---|
| Form | One long-running service on the host, a binary from this workspace (`crates/barre`), installed and started by Ansible as a systemd unit. A CLI, the same binary, manages policies. Built on tokio, like jete. |
| Policies | Files in `/run/barre/policies/`, root-only, written by `barre allow` and removed by `barre revoke`. `/run` is cleared at boot, like the VMs. barre reloads them when they change. |
| Listening | For each policy, barre listens on that VM's `.1`, on its one port (TCP and UDP), and stops when the policy is revoked. Nothing listens on a wildcard address, so barre is unreachable from the LAN. |
| DNS | Allowed names: an `A` record with the name's synthetic address, short TTL. Allowed names, other record types (`AAAA`, `HTTPS`, `SVCB`, ...): an empty answer, so clients use IPv4 and don't get encrypted-handshake keys (see ECH below). Everything else: `NXDOMAIN`. Nothing is forwarded upstream for names that aren't allowed, so DNS can't carry data out. |
| Destinations | barre **refuses non-public addresses** for names: an allowed name that resolves to the LAN, the tailnet, loopback, jete's pool or any other private range is refused, so DNS can't point a VM's traffic at the host's neighbours. An address rule reaches exactly what it names. |
| Refusals | A refused connection is closed at once, and logged with the reason. |
| Logging | One JSON line per DNS query and per connection, to the journal: time, VM id and address, name, original destination, port, decision and reason, bytes each way, duration. |
| Limits | Per VM: concurrent connections (default 256) and an idle timeout (default 10 minutes). A handshake or request that doesn't arrive within 10 seconds is dropped. |
| Hardening | A dedicated `barre` user with only `CAP_NET_BIND_SERVICE`, plus systemd sandboxing (read-only filesystem except its runtime directory, no new privileges, a system call filter). It holds no secrets in the first phase. |
| Guest image | Adds `curl`, which agents need anyway, and the tests use to make real HTTPS requests. |

## Known limits

- **Shared hosting.** On 443, barre allows a connection by name, then passes bytes it can't read.
  A client can name an allowed host in the handshake and then ask the same server for a different
  site inside the encrypted stream ("domain fronting"). Some CDNs refuse this, others don't.
  Closing it needs barre to decrypt, which phase 2 does only for hosts with credentials. For the
  rest, the risk is limited to sites served by the same provider as an allowed one.
- **Encrypted Client Hello (ECH).** With ECH, the real name is encrypted and the handshake shows
  only the provider's public name. Clients need keys from DNS (`HTTPS` records) to use it, and barre
  never returns those records.
- **Other ports aren't inspected.** A name allowed on port 22 can carry anything over port 22. The
  rule says where traffic may go, not what it is.
- **UDP is DNS only.** Protocols that need other UDP don't work through barre.

## Phase 2: credentials

For hosts that need credentials (the model API, GitHub), barre terminates TLS with a certificate
from its own certificate authority, which the guest image trusts, adds the credential to each
request (for example an `Authorization` or `x-api-key` header), and opens its own TLS connection to
the real host. Policies name the credentials a VM may use, never their values. The values live on
the host, readable only by barre, and come from outside the repo, like every secret. Every other
host stays pass-through. This is designed in detail when we build it.

## Milestones

| | Goal | Done when |
|---|---|---|
| G1 | By name | A VM with `--egress-proxy` reaches allowed names, on allowed ports, and nothing else; every query and connection is logged; revoking a policy cuts the VM off. |
| G2 | Credentials | A VM calls an API that needs a key without holding the key. |

**G1 is done when**, on hemlock:

```bash
id=$(sudo jete run --egress-proxy 15001)
addr=$(sudo jete ls --json | jq -r ".[] | select(.id==\"$id\") | .network.host")
sudo barre allow "$addr" --vm "$id" example.com
sudo jete exec "$id" -- curl -sI https://example.com                  # 200
sudo jete exec "$id" -- curl -sI https://github.com                   # fails: doesn't resolve
sudo jete exec "$id" -- curl -sI http://example.com                   # fails: port 80 not allowed
sudo jete exec "$id" -- bash -c 'echo > /dev/tcp/1.1.1.1/443'        # fails: no rule for the address
journalctl -u barre                                                    # all four, with reasons
sudo barre revoke "$addr"
sudo jete exec "$id" -- curl -sI https://example.com                  # fails
```

The `jq` line and the manual `allow`/`revoke` are what the control plane will do later, in one
step with `jete run` and `jete rm`.

## Tests

- **jete, on hemlock, with a stand-in listener instead of barre:** with `--egress-proxy`, TCP to
  any address and port, and UDP to port 53, arrive at the listener with their original
  destination; other UDP and ICMP don't arrive; nothing is forwarded; the host's other ports and
  other VMs aren't reachable; `--egress-proxy` with `--allow` is refused.
- **barre, unit:** SNI parsing (whole, split across reads, malformed, missing, oversized), DNS
  answers per record type, rule parsing and matching, synthetic address allocation, policy files,
  refusal of non-public addresses.
- **barre, on hemlock, with VMs:**
  - allowed names work on their ports (HTTPS, HTTP when allowed, another TCP port);
  - names not allowed neither resolve nor connect; a real address connects only with an address
    rule; a mismatched SNI is refused;
  - an allowed name that resolves to a private address is refused;
  - one VM's policy and synthetic addresses don't work for another;
  - nothing gets through when the VM has no policy, or after `revoke`;
  - every case is logged.

## Open questions

1. **Who calls `allow` and `revoke` before the control plane exists.** A small wrapper script, or a
   `jete run` hook. A hook would make jete aware of the gateway, which the layering says it
   shouldn't be, so the proposal is a script.
