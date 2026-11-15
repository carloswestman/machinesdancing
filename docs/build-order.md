# Build order

What we build, in what order, and where each step stands. [architecture.md](architecture.md) says
what the pieces are; this says when they come.

The aim is a thin end-to-end slice early (one worker, one ticket, one pull request), then widening.
The first real work is on this repo: machinesdancing is built with machinesdancing, as soon as it
can do a task end to end. Which repos an agent works on is configuration, not code, so the same
harness can work on other repos later (see [Instances](#instances)).

| Step | Delivers | Status |
|---|---|---|
| 1. jete M2: Network | A µVM's network link reaches only the addresses it's given, and nothing on the host, the LAN, the tailnet or other µVMs. No allowlist by name in jete: that is the gateway's job. | Done. |
| 2. barre G1: By name | The egress gateway: allowlist by name and port, logging. | Done. |
| 3. One agent in a µVM | An agent program (Claude Code or OpenCode) in a guest image, reaching its model through barre, doing a task started by hand. | Started: Claude Code in the guest image, behind barre, running prompts started by hand ([claude-code.md](guides/claude-code.md)). |
| 4. Repo access | The agent works with git and `gh` as a developer does, on the repos its instance names, with the access the instance gives it. | Not started. |
| 5. barre G2: Credentials | barre adds the GitHub and model credentials on the way out, so the µVM holds no secret at all. | Not started. |
| 6. Bus (mime) | The shim and the host bus, with channels and schemas from configuration. | Not started. |
| 7. Control plane (regisseur) | The work repo, the workflow engine with its first rules, the GitHub connector, spawning on tickets. The first slice that runs without a human starting it. | Not started. |
| 8. Widening | More rules, the Jira connector, a skills disk, jete's daemon and speed (snapshots, a warm pool) when spawning on demand needs them. | Not started. |

Steps 3 and 4 finish together: step 3 is done when a real task, from a ticket written as for a
person, ends in a pull request that the human reviews and merges. That needs step 4.

## Instances

An **instance** is one team's setup: the repos its agents work on, what each agent may do there,
what its µVMs may reach, and which credentials they use, by name. It's the *instance config* store
of [architecture.md](architecture.md#where-things-are-stored). It is never committed to this repo:
this repo holds the tools, generic and open source, with no one's setup in it (principle 4). An
instance is a repo of its own, or a directory of configuration on the host.

- **Today:** one instance, whose only repo is machinesdancing. Its configuration, too, lives
  outside this repo.
- **Later:** more instances, each with its own repos, GitHub identity and credentials. Nothing in
  the tools changes; the instance does.

The instance config's format starts as small as step 4 needs and grows with each step: roles and
channels with mime, rules with regisseur.

## Step 4: repo access

The agent is a developer: it clones, branches, commits, pushes and opens pull requests with `git`
and `gh`, the tools it already knows well. No extra tool wraps git, and no tokens are spent on one.
What it may do is decided by what it holds and where it can reach, not by what it's told:

| Limit | Enforced by |
|---|---|
| Which repos | The credential: a GitHub App installed on the instance's repos only, or a fine-grained token scoped to them. |
| What it may do there | The credential's permissions: contents and pull requests, read and write. No administration, no `workflows`, so a push that changes `.github/workflows/` is refused by GitHub. |
| No merging into `main` | Branch protection or a ruleset on the instance's repos: changes reach `main` only by pull request, with the human's review (principle 8). |
| Where it can reach | barre: `github.com` and `api.github.com` added to the µVM's rules, by the instance. |
| Who it is | Its own GitHub identity, not the human's: the App's bot user, or a machine user. The human can then review and approve its pull requests, and its actions are recorded as its own. |

A GitHub App is the better default: its tokens are minted on the host for an hour at a time, scoped
to the instance's repos, and the agent shows up as itself. Until G2, the µVM holds such a token in
memory, the way it holds the Claude token today: sent over stdin, never on a command line or a disk.
G2 then takes it out.

## Building the harness with the harness

In the first instance the agent works on the code that contains it. That is safe as long as nothing
the agent does takes effect before the human acts:

- **The agent can't merge** (step 4's branch protection).
- **New code runs only when the human installs it.** hemlock changes only through Ansible, which
  the human applies. A µVM runs the binaries already installed, never the ones it just wrote.
- **No CI runs the agent's code with secrets.** The repo has no CI today, and without the
  `workflows` permission the agent can't add any.
- **Review where it matters most.** A subtle change to barre's policy code or jete's network setup
  is the hardest to catch. The first tickets are elsewhere: docs, scripts, tests.

## Step 5: barre G2

After step 4 the µVM holds two secrets: the Claude token and the GitHub token. With G2 barre adds
credentials to requests to the hosts that need them, and the µVM holds none
([barre.md](design/barre.md#milestones)). Two things to check when we build it: whether a
subscription token can be added this way, or only an API key, and how git over HTTPS behaves
through it.

## Known gaps

Small things that don't block a step, recorded so they're not forgotten:

- **barre picks up a policy within half a second.** `barre-vm run` returns before that, so a
  command run at once may be refused. The demos start with `sleep 1`. `barre-vm run` should wait
  until barre has the policy loaded; this matters once tasks start without a human watching.
