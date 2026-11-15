# Build order

What we build, in what order, and where each step stands. [architecture.md](architecture.md) says
what the pieces are; this says when they come.

The aim is a thin end-to-end slice early (one worker, one ticket, one pull request), then widening.
The first real work is on this repo: machinesdancing is built with machinesdancing, as soon as it
can do a task end to end (see [The first real task](#the-first-real-task)).

| Step | Delivers | Status |
|---|---|---|
| 1. jete M2: Network | A µVM's network link reaches only the addresses it's given, and nothing on the host, the LAN, the tailnet or other µVMs. No allowlist by name in jete: that is the gateway's job. | Done. |
| 2. barre G1: By name | The egress gateway: allowlist by name and port, logging. | Done. |
| 3. One agent in a µVM | An agent program (Claude Code or OpenCode) in a guest image, reaching its model through barre, doing a task started by hand. | Started: Claude Code in the guest image, behind barre, running prompts started by hand ([claude-code.md](guides/claude-code.md)). |
| 4. Git through the host | The agent commits in its µVM and holds no git credential. The host takes its branch out, checks it, pushes it and opens the pull request. The first action gate. | Not started. |
| 5. barre G2: Credentials | barre adds the model's key on the way out, so the µVM holds no key at all. | Not started. |
| 6. Bus (mime) | The shim and the host bus, with channels and schemas from configuration. The gate from step 4 becomes the bus's first action. | Not started. |
| 7. Control plane (regisseur) | The work repo, the workflow engine with its first rules, the GitHub connector, spawning on tickets. The first slice that runs without a human starting it. | Not started. |
| 8. Widening | More rules, the Jira connector, a skills disk, jete's daemon and speed (snapshots, a warm pool) when spawning on demand needs them. | Not started. |

Steps 3 and 4 finish together: step 3 is done when a real task ends in a pull request, and that
needs step 4.

## The first real task

A ticket from this repo, written as for a person, done by Claude Code in a µVM, delivered as a pull
request that the human reviews and merges. By hand: a script starts it, not the control plane.

Why this repo, and not a ticket from other work:

- **No one else's code leaves its home.** Work code on a home lab, through a personal
  subscription, is a question for that work's owner and the subscription's terms (architecture's
  open question 2). This repo has no such question.
- **We can judge the result.** We know the code, so we can tell a good pull request from a
  plausible one.
- **What's missing shows up where we can fix it.** The task will find the gaps (git access, how
  results leave the µVM, what the agent needs to know about the repo), and the fix is in the same
  repo.

### Building the harness with the harness

The agent works on the code that contains it. That is safe as long as nothing the agent does takes
effect before the human acts:

- **The agent can't merge.** Branch protection on `main` requires the human's review (principle 8).
  The host's token can push branches and open pull requests, not merge.
- **New code runs only when the human installs it.** hemlock changes only through Ansible, which
  the human applies. A µVM runs the binaries already installed, never the ones it just wrote.
- **No CI runs the agent's code with secrets.** The repo has no CI today. When it gets some, a
  workflow on a pull request from a branch in the same repo can read the repo's secrets, so the gate
  rejects branches that change `.github/` (step 4).
- **Review where it matters most.** A subtle change to barre's policy code or jete's network setup
  is the hardest to catch. The first tickets are elsewhere: docs, scripts, tests.

## Step 4: git through the host

Git is how work is delivered (principle 10), so it's the first thing the system does for the agent
instead of the agent doing it. The agent gets less privilege than a developer would: it can commit,
and nothing else.

- **In:** the host makes a bundle of the repo at the ticket's base commit and sends it into the µVM
  through jete (`jete exec`, over vsock). The agent clones from it. The µVM needs no route to
  GitHub, so barre allows `api.anthropic.com` and nothing else.
- **Work:** the agent commits on a branch, in its µVM.
- **Out:** the host asks jete-agent for a bundle of that branch. Identity comes from the
  connection: the host knows which µVM, and so which ticket, it's talking to (principle 7). Nothing
  the agent says about itself counts.
- **Check:** a deterministic policy decides whether the branch may become a pull request, and
  rejects it with a reason otherwise (principle 6). For example: the branch is `agent/<ticket>`, it
  descends from the base commit, it changes nothing under `.github/`, it's within a size limit.
- **Deliver:** the host pushes the branch with its own token, scoped to this repo, and opens the
  pull request, which links the ticket and the µVM.

This is the action gate in its smallest form: one action ("submit my branch"), one policy, identity
from the hypervisor. In step 6 the agent asks for it through mime instead of a script.

## Step 5: barre G2

After step 4 the µVM holds one secret: the Claude token. With G2 barre adds the credential to
requests to the model's API, and the µVM holds none ([barre.md](design/barre.md#milestones)).
Whether a subscription token can be added this way, or only an API key, is checked when we build
it.

## Known gaps

Small things that don't block a step, recorded so they're not forgotten:

- **barre picks up a policy within half a second.** `barre-vm run` returns before that, so a
  command run at once may be refused. The demos start with `sleep 1`. `barre-vm run` should wait
  until barre has the policy loaded; this matters once tasks start without a human watching.
