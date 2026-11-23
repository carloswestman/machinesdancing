# barre — design

barre is the egress gateway: it carries every connection a VM makes to the outside world. It
allows each connection by the **name** the VM asked for, checked against that VM's allowlist; logs
every request; and, in a later phase, adds credentials so agents never hold them. Its place in the
whole is in [architecture.md](../architecture.md); the network it sits on is jete's M2
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
- From G2: add credentials to requests for the names a VM's policy says, so the VM never holds
  them (see [G2: credentials](#g2-credentials)).

**Does not**

- Know about agents, roles or messages. It sees policies per address: "this address may reach these
  names, on these ports".
- Create VMs or networks: jete does. barre doesn't read jete's state either; it's told what to
  allow.
- Decrypt traffic, except for names a policy gives a credential (from G2). Rules apply to whole
  hosts and ports, not URL paths.
- Know what a credential is for. It adds a header it's given, to the names it's told; minting and
  refreshing tokens is the host's job.

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
| Policies | Files in `/run/barre/policies/`, one per address, written by root with `barre allow` and removed with `barre revoke`; the service can only read them. `/run` is cleared at boot, like the VMs. barre picks up changes within half a second. Each policy records the interface that carried its address when it was written, by name and index, and applies only while that interface exists. A VM's interface goes when jete removes it, and a new interface given the same address has a new index, so a stale policy is inert; `allow`, `revoke` and `ls` delete stale ones. |
| Listening | For each policy, barre listens on that VM's `.1`, on its one port (TCP and UDP), and stops when the policy is revoked. Nothing listens on a wildcard address, so barre is unreachable from the LAN. |
| DNS | Allowed names: an `A` record with the name's synthetic address, short TTL. Allowed names, other record types (`AAAA`, `HTTPS`, `SVCB`, ...): an empty answer, so clients use IPv4 and don't get encrypted-handshake keys (see ECH below). Everything else: `NXDOMAIN`. Nothing is forwarded upstream for names that aren't allowed, so DNS can't carry data out. |
| Destinations | barre **refuses non-public addresses** for names: an allowed name that resolves to the LAN, the tailnet, loopback, jete's pool or any other private range is refused, so DNS can't point a VM's traffic at the host's neighbours. An address rule reaches exactly what it names. |
| Refusals | A refused connection is closed at once, and logged with the reason. |
| Logging | One JSON line per DNS query and per connection, to the journal: time, VM id and address, name, original destination, port, decision and reason, bytes each way, duration. |
| Limits | Per VM: concurrent connections (default 256) and an idle timeout (default 10 minutes). A handshake or request that doesn't arrive within 10 seconds is dropped. |
| Hardening | A dedicated `barre` user with no capabilities (its port is above 1024), plus systemd sandboxing: read-only filesystem, no new privileges, only IPv4 and Unix sockets, a system call filter. Until G2 it holds no secrets; from G2, the private key of its certificate authority and the credentials of running VMs, readable by it alone. |
| Guest image | Adds `curl`, which agents need anyway, and the tests use to make real HTTPS requests. |

## Known limits

- **Shared hosting.** On 443, barre allows a connection by name, then passes bytes it can't read.
  A client can name an allowed host in the handshake and then ask the same server for a different
  site inside the encrypted stream ("domain fronting"). Some CDNs refuse this, others don't.
  Closing it needs barre to decrypt, which G2 does only for names with credentials. For the
  rest, the risk is limited to sites served by the same provider as an allowed one.
- **Encrypted Client Hello (ECH).** With ECH, the real name is encrypted and the handshake shows
  only the provider's public name. Clients need keys from DNS (`HTTPS` records) to use it, and barre
  never returns those records.
- **Other ports aren't inspected.** A name allowed on port 22 can carry anything over port 22. The
  rule says where traffic may go, not what it is.
- **UDP is DNS only.** Protocols that need other UDP don't work through barre.

## G2: credentials

The VM holds no secret: no model token, no GitHub token. For the names that need one, barre
terminates the VM's TLS connection, sets the credential's header on each request, and makes its own
TLS connection to the real host. The agent still uses the credential, through barre, toward the
names its policy allows; what it can't do is read it, keep it, or take it anywhere else.

### Policies name credentials

A rule may name a credential after `@`:

```bash
sudo barre allow 10.200.0.5 --vm 3f2a9c1b \
    api.anthropic.com@claude github.com@github-git api.github.com@github-api '*.githubusercontent.com'
```

`NAME[:PORTS]@CREDENTIAL`, for an exact name, on port 443 only. Names without `@` stay as in G1:
passed through, unread. A credential is a header, set for one VM, by root, from stdin:

```bash
printf 'Authorization: Bearer %s\n' "$token" | sudo barre credential 10.200.0.5 claude
sudo barre credential 10.200.0.5 claude --rm
```

It's a file in `/run/barre/credentials/<address>/<name>`, written by root, readable by barre alone,
removed with the VM's policy. barre re-reads it on each change, like policies, so the host replaces
a token that is about to expire without touching the VM.

barre knows nothing about Anthropic or GitHub: it sets the header it's given. The host decides
what goes in it, and keeps it fresh: `claude-vm` now (and regisseur later) writes
`Authorization: Bearer <OAuth token>` for `api.anthropic.com`, `Authorization: Basic
<x-access-token:App token>` for git on `github.com`, and `Authorization: Bearer <App token>` for
`api.github.com`, replacing the GitHub ones every 50 minutes as today
([repo-access.md](repo-access.md#how-a-task-gets-access)).

### What barre does with those connections

1. **Terminate TLS** with a certificate for the name, made on the fly and signed by barre's
   certificate authority. Its ALPN offers HTTP/1.1 only, so every client speaks a protocol barre
   reads one request at a time.
2. **Check the name.** The SNI and each request's `Host` must be the rule's name. Anything else is
   refused, which also closes domain fronting for these names.
3. **Set the credential.** Remove every header the client sent with the credential's header name,
   then add the credential's. Nothing the VM sends can choose or see the real value.
4. **Forward** the request over barre's own TLS connection to the real host, verified against the
   public certificate authorities, and stream the response back as it comes (model responses are
   streamed).
5. **Log each request:** method, host, path, status, bytes, duration. The header's value is never
   logged.

### The certificate authority

| | |
|---|---|
| Where | Made once per host by `barre ca init`, run by Ansible: a key that never leaves the host (`/etc/barre/ca.key`, readable by barre alone) and its certificate (`/etc/barre/ca.crt`, public). |
| Trusted by | The guest image: the build, which runs on the host, adds `ca.crt` to the system's trust store (curl, git and gh use it). Claude Code bundles its own list, and is pointed at the file with `NODE_EXTRA_CA_CERTS`. |
| Scope | It signs a certificate only for a name a policy gives a credential, for that VM's connection. A VM can't make barre impersonate any other name. |

### Building it

TLS is rustls, with `ring` for cryptography. `ring` compiles C, and the Mac builds for
`x86_64-unknown-linux-musl` with `rust-lld` alone, so the build gains a C cross compiler: zig,
through `cargo-zigbuild` (`brew install zig`, `cargo install cargo-zigbuild`). Binaries stay
static, built on the Mac, run on hemlock. A pure-Rust provider (RustCrypto's) was the alternative;
`ring` is the better tested.

### In the VM

Programs still need something to send, or they refuse to start, so they get placeholders:
`CLAUDE_CODE_OAUTH_TOKEN=placeholder`, a `GH_TOKEN` and a git credential helper with the same.
barre replaces them on the way out. `/run/agent/github-token` goes away.

### Known limits

- **The agent can use what it can't hold.** Through barre, it can call the APIs its credentials
  open, as far as their scope allows. G2 stops a token leaving the VM, not its use from inside;
  narrow scopes (one repo, one hour) and the merge rule are what limit that.
- **HTTP/1.1 only** for these names. Clients that insist on HTTP/2 won't connect. git, gh, curl and
  Claude Code all fall back.
- **A new certificate authority means a new image.** Rotating `ca.key` needs the guest image rebuilt.

## Milestones

| | Goal | Done when |
|---|---|---|
| G1 ✓ | By name | A VM with `--egress-proxy` reaches allowed names, on allowed ports, and nothing else; every query and connection is logged; revoking a policy cuts the VM off. |
| G2 | Credentials | A VM runs Claude Code on a ticket, through to a pull request, with no secret in the VM: the Claude and GitHub tokens are set by barre. Searching the VM's memory-backed files and environment for either token finds nothing. Every request to those names is logged. |

**G1 is done when**, on hemlock:

```bash
id=$(sudo barre-vm run example.com github.com:22)
sudo jete exec "$id" -- curl -sI https://example.com                  # 200
sudo jete exec "$id" -- bash -c 'exec 3<>/dev/tcp/github.com/22; head -c 8 <&3'   # SSH-2.0-
sudo jete exec "$id" -- curl -sI https://github.io                    # fails: doesn't resolve
sudo jete exec "$id" -- curl -sI http://example.com                   # fails: port 80 not allowed
sudo jete exec "$id" -- curl -sI https://1.1.1.1                      # fails: no rule for the address
journalctl -u barre -o cat                                             # each one, with reasons
sudo barre-vm rm "$id"
```

`barre-vm` (installed with barre in `/usr/local/bin`; source in
`ansible/roles/barre/templates/barre-vm.j2`) runs `jete run --egress-proxy`, then `barre allow` with the VM's
address; `barre-vm rm` revokes the policy and removes the VM. The control plane will do the same
later.

Done: `crates/barre/tests/vm.rs` checks the above with real VMs and barre running in the test, plus
an allowed address, a mismatched TLS server name, a name that resolves to a private address, an
address barre never gave out, one VM using another's synthetic address, and a new VM given a
removed VM's address. Every case is logged with its reason. The service also ran once on hemlock
under its systemd sandbox, behind the installed jete.

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

1. **Claude Code and `NODE_EXTRA_CA_CERTS`.** Its native build is documented to honour it; check in
   a VM first, since nothing else works without it.
2. **git with an added `Authorization`.** git sends credentials only after a 401; with barre adding
   them to every request, the first one succeeds. Check that fetch and push both work, and that a
   placeholder helper is still needed.

For G1, none. Who calls `allow` and `revoke` before the control plane exists was settled as a
script, `barre-vm`, so jete stays unaware of the gateway.
