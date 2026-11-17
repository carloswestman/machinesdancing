# Security

machinesdancing exists to contain AI agents, so a way out of that containment is the kind of bug
that matters most here: a µVM reaching something barre should have refused, a guest gaining control
of the host or of another µVM, a secret leaving where it is kept, or jete-agent's protocol being
usable from inside the guest.

## Reporting a vulnerability

Please report it privately, not in a public issue: use **Report a vulnerability** on the
repository's [Security tab](https://github.com/carloswestman/machinesdancing/security/advisories/new).
Include what you did, what you expected the system to refuse, and what happened instead.

I'll acknowledge the report within a week. This is a one-person project, so a fix may take longer;
I'll keep you posted and credit you when it is published, if you wish.

## Scope

The project is experimental and not for production (see the [README](README.md)). Known limits,
such as domain fronting through shared hosts, are documented in
[docs/design/barre.md](docs/design/barre.md); reports that go beyond them are especially welcome.
Vulnerabilities in Firecracker, KVM or the Linux kernel themselves belong upstream; tell me too if
they affect how this project uses them.
