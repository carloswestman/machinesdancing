# Egress gateway — design

The egress gateway carries every connection a VM makes to the outside world. It allows each
connection by the **name** the VM asks for, checked against that VM's allowlist; logs every
request; and, in a later phase, adds credentials so agents never hold them. Its place in the whole
is in [architecture.md](architecture.md); the network it sits on is jete's M2
([jete.md](jete.md#m2-plan)).

Working name: **barre** (the rail dancers hold on to: something to lean on, and a barrier). The name
is a proposal.

## Scope

**Does**

- Answer a VM's DNS: names on its allowlist resolve, everything else doesn't.
- Accept the VM's HTTPS and HTTP connections, read which name each one asks for, and pass it
  through to that name only if it is allowed.
- Log every DNS answer and connection, allowed or not.
- Later: add credentials to requests for configured hosts.

**Does not**

- Know about agents, roles or messages. It sees policies per address: "this address may reach these
  names".
- Create VMs or networks: jete does. barre doesn't read jete's state either; it's told what to
  allow.
- Decrypt traffic in the first phase. Rules apply to whole hosts, not URL paths.
- Carry anything but DNS, HTTP and HTTPS. Other protocols (git over SSH, raw TCP) use jete's
  address allowlist, `--allow`.

## How a VM uses it

A VM that goes through the gateway gets **no route to the internet at all**: no `--allow`. It can
reach only three ports on its own host-side address (`.1`), where barre listens:

```bash
id=$(sudo jete run --host-port 53 --host-port 80 --host-port 443 --dns host)
addr=$(sudo jete ls --json | jq -r ".[] | select(.id==\"$id\") | .network.host")
sudo barre allow "$addr" --vm "$id" github.com '*.githubusercontent.com' api.anthropic.com
# ... the VM works ...
sudo barre revoke "$addr"
sudo jete rm "$id"
```

Inside the VM:

1. The guest asks its resolver (`.1`, from `--dns host`) for `github.com`.
2. barre checks the name against the allowlist for the address the query arrived on. Allowed: it
   answers with **its own address**, `.1`. Not allowed: no such name.
3. The guest connects to `.1:443`, sending `github.com` in the TLS handshake, as every HTTPS
   client does (SNI).
4. barre reads the name from the handshake, checks it again, resolves it itself through the host's
   resolver, connects, and from then on copies bytes both ways without reading them.

So every allowed connection goes through barre, by construction: the VM has no other way out.
Code that ignores DNS and connects to an address directly gets nowhere.

The `jq` line and the manual `allow`/`revoke` are what the control plane will do later, in one
step with `jete run` and `jete rm`.

## Decisions

| Concern | Decision |
|---|---|
| Form | One long-running service on the host, a binary from this workspace (`crates/barre`), installed and started by Ansible as a systemd unit. A CLI, the same binary, manages policies. Built on tokio, like jete. |
| Policies | One per VM, keyed by the VM's host-side address: the names it may reach, and the VM's id for the logs. Files in `/run/barre/policies/`, root-only, written by `barre allow` and removed by `barre revoke`. `/run` is cleared at boot, like the VMs. barre reloads them when they change. A policy is also dropped as soon as its address disappears from the host (jete deleted the VM's tap), so a VM removed without `revoke` can't pass its policy on to the next VM given that address. |
| Listening | For each policy, barre listens on that address only, on ports 53 (UDP and TCP), 80 and 443, and stops when the policy is revoked. Nothing listens on a wildcard address, so barre is unreachable from the LAN, and which address a connection arrived on identifies the VM: jete's firewall drops any packet a VM sends from an address that isn't its own. |
| Names | An allowlist entry is an exact name (`github.com`) or a wildcard for its subdomains (`*.githubusercontent.com`, which doesn't include the bare name). Matching is case-insensitive, on the name without a trailing dot. |
| DNS | Allowed names: an `A` record with barre's own address, short TTL. Allowed names, other record types (`AAAA`, `HTTPS`, `SVCB`, ...): an empty answer, so clients use IPv4 and don't get encrypted-handshake keys (see ECH below). Everything else: `NXDOMAIN`. Nothing is forwarded upstream for names that aren't allowed, so DNS can't carry data out. |
| HTTPS | barre reads the TLS ClientHello (up to 16 KiB), takes the name from its SNI extension, and allows or refuses the connection. No SNI: refused. Then it connects to the name on port 443 and splices the streams; the handshake it read is passed on unchanged. |
| HTTP | barre reads the request line and headers (up to 16 KiB) and allows by the `Host` header, then splices. Off by default per policy (`--http`), because plain HTTP lets anyone on the path change it. |
| Destinations | barre resolves names through the host's resolver and **refuses non-public addresses**: an allowed name that resolves to the LAN, the tailnet, loopback, jete's pool or any other private range is refused, so DNS can't be used to point a VM's traffic at the host's neighbours. |
| Logging | One JSON line per DNS query and per connection, to the journal: time, VM id and address, name, port, decision and reason, bytes each way, duration. |
| Limits | Per VM: concurrent connections (default 256) and an idle timeout (default 10 minutes). A handshake that doesn't arrive within 10 seconds is dropped. |
| Hardening | A dedicated `barre` user with only `CAP_NET_BIND_SERVICE`, plus systemd sandboxing (read-only filesystem except its runtime directory, no new privileges, a system call filter). It holds no secrets in the first phase. |
| jete change | `--dns host`: use the VM's own host-side address as its resolver. The address isn't known until jete allocates it. |
| Guest image | Adds `curl`, which agents need anyway, and the tests use to make real HTTPS requests. |

## Known limits

- **Shared hosting.** barre allows a connection by the name in the handshake, then passes bytes it
  can't read. A client can name an allowed host in the handshake and then ask the same server for a
  different site inside the encrypted stream ("domain fronting"). Some CDNs refuse this, others
  don't. Closing it needs barre to decrypt, which phase 2 does only for hosts with credentials. For
  the rest, the risk is limited to sites served by the same provider as an allowed one.
- **Encrypted Client Hello (ECH).** With ECH, the real name is encrypted and the handshake shows
  only the provider's public name. Clients need keys from DNS (`HTTPS` records) to use it, and barre
  never returns those records. A client that has keys from elsewhere can only reach a name barre
  allows in the visible handshake.
- **Not every client sends SNI.** Those are refused, by design.

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
| G1 | By name | A VM with only the gateway's ports reaches allowed names over HTTPS and nothing else; every query and connection is logged; revoking a policy cuts the VM off. |
| G2 | Credentials | A VM calls an API that needs a key without holding the key. |

**G1 is done when**, on hemlock:

```bash
id=$(sudo jete run --host-port 53 --host-port 80 --host-port 443 --dns host)
sudo barre allow <its address> --vm "$id" example.com
sudo jete exec "$id" -- curl -sI https://example.com         # 200
sudo jete exec "$id" -- curl -sI https://github.com          # fails: doesn't resolve
sudo jete exec "$id" -- curl -sI --resolve github.com:443:<its address> https://github.com
                                                              # fails: refused by name
journalctl -u barre                                           # all three, with reasons
```

## Tests

- **Unit:** SNI parsing (whole, split across reads, malformed, missing, oversized), DNS answers per
  record type, name matching, policy files, refusal of non-public addresses.
- **On hemlock, with VMs:**
  - allowed names work over HTTPS (and HTTP when enabled);
  - names not allowed neither resolve nor connect, even when the VM forces the address;
  - an allowed name that resolves to a private address is refused;
  - one VM's policy doesn't apply to another;
  - nothing answers when the VM has no policy, or after `revoke`;
  - every case is logged.

## Open questions

1. **The name.** barre, or something else.
2. **Who calls `allow` and `revoke` before the control plane exists.** A small wrapper script, or a
   `jete run` hook. A hook would make jete aware of the gateway, which the layering says it
   shouldn't be, so the proposal is a script.
