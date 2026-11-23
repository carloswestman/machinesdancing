# Instances — design

An **instance** is one team's setup of machinesdancing: the repos its agents work on, the roles
they take, what each role may reach and do, and which credentials it uses, by name. The tools in
this repo are generic and open source; an instance is everything that makes them one team's
harness. It's the *instance config* store of [architecture.md](../architecture.md#where-things-are-stored).

## Scope

**Holds**

- The instance's repos, and which role works on each.
- Roles: what an agent in each role may reach (barre rules) and which credentials it uses.
- Later: channels and message schemas (mime), workflow rules (regisseur), the skills each role is
  given.

**Does not hold**

- Secrets. It names credentials; their values live on the host (see [Credentials](#credentials)).
- Anything about the tools themselves: how jete, barre or the scripts work is this repo's.

## Decisions

| Topic | Decision |
|---|---|
| Never in this repo | No instance is committed here, not even the one that works on machinesdancing itself. This repo says nothing about any team's setup (principle 4). |
| A repo of its own | An instance is a git repo, usually private. Its changes are reviewed like code (principle 15). A plain directory works too, for trying things out. |
| Found by path | The tools find the instance through `MD_INSTANCE`, the path of a checkout. Today the scripts on the Mac read it; when regisseur runs on the host, the host has its own checkout. |
| One file to start | `instance.toml` at the instance's root. It grows a section per step: roles and repos for repo access, channels with mime, rules with regisseur. |
| Many instances | Nothing in the tools assumes there is only one. Each instance has its own repos, its own GitHub identities and its own credentials. |

## Format

What step 4 ([repo-access.md](repo-access.md)) needs, and no more:

```toml
# instance.toml
name = "example"

[roles.worker]
github_app = "example-worker"        # a credential name, not a value
allow = ["api.anthropic.com", "github.com", "api.github.com"]   # barre rules

[[repos]]
url = "https://github.com/example-org/example"
roles = ["worker"]
```

- `roles.<name>.allow` is a list of barre rules, in barre's syntax (`name[:ports]`).
- `roles.<name>.github_app` names the GitHub App the role acts as.
- `repos` lists the repos the instance works on, and which roles may work on each.

The format is a starting point and will change as we build; nothing reads it yet.

## Credentials

The instance names credentials; the host holds their values.

- **Where:** files on hemlock under `/etc/machinesdancing/credentials/<name>/`, readable only by
  root (and later by barre, for G2).
- **Who puts them there:** the human, from outside both repos. Neither this repo nor the instance
  holds a secret. The `credentials` Ansible role makes the directory; for a GitHub App,
  `scripts/install-github-app SLUG` copies the key from the Mac, over ssh's stdin.
- **Who reads them:** the host, when it starts an agent: it mints what the agent needs from them
  (for GitHub, a token that lasts an hour). The agent never sees the long-lived secret.

## Open questions

1. **Getting values onto the host.** By hand, or Ansible with a vault kept in the instance repo.
   hemlock is configured only through Ansible, which favours the vault.
2. **The instance on the host.** When regisseur reads the instance on the host, how its checkout is
   kept current: pulled on merge, or deployed like the rest of the host's configuration.
