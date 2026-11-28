# Build order

What we build, in what order, and where each step stands. [architecture.md](architecture.md) says
what the pieces are; this says when they come.

The aim is a thin end-to-end slice early (one worker, one ticket, one pull request), then widening.
The first real work is on this repo: machinesdancing is built with machinesdancing, as soon as it
can do a task end to end. Which repos an agent works on is configuration, not code, so the same
harness can work on other repos later ([instances.md](design/instances.md)).

| Step | Delivers | Status |
|---|---|---|
| 1. jete M2: Network | A µVM's network link reaches only the addresses it's given, and nothing on the host, the LAN, the tailnet or other µVMs. No allowlist by name in jete: that is the gateway's job. | Done. |
| 2. barre G1: By name | The egress gateway: allowlist by name and port, logging. | Done. |
| 3. One agent in a µVM | An agent program (Claude Code or OpenCode) in a guest image, reaching its model through barre, doing a task started by hand. | Done: Claude Code in the guest image, behind barre ([claude-code.md](guides/claude-code.md)). |
| 4. Repo access | The agent works with git and `gh` as a developer does, on the repos its instance names, as a GitHub App for its role: [repo-access.md](design/repo-access.md), configured by the instance: [instances.md](design/instances.md). | Done: the first ticket became #29, opened by the agent and merged by the human ([milestone](design/repo-access.md#milestone)). |
| 5. barre G2: Credentials | barre adds the GitHub and model credentials on the way out, so the µVM holds no secret at all. | Done: the Claude and GitHub tokens are set by barre; a ticket became #35 with neither in the µVM ([milestone](design/barre.md#milestones)). |
| 6. Bus (mime) | The `mime` CLI in the µVM and the host bus, with channels and schemas from configuration; messages delivered to the agent between turns. | Designed: [mime.md](design/mime.md). |
| 7. Control plane (regisseur) | The work repo, the workflow engine with its first rules, the GitHub connector, spawning on tickets. The first slice that runs without a human starting it. | Not started. |
| 8. Widening | More rules, the Jira connector, a skills disk, jete's daemon and speed (snapshots, a warm pool) when spawning on demand needs them. | Not started. |

Steps 3 and 4 finish together: step 3 is done when a real task, from a ticket written as for a
person, ends in a pull request that the human reviews and merges. That needs step 4.

## Building the harness with the harness

In the first instance the agent works on the code that contains it. That is safe as long as nothing
the agent does takes effect before the human acts:

- **The agent can't merge** ([repo-access.md](design/repo-access.md#trust-model)).
- **New code runs only when the human installs it.** hemlock changes only through Ansible, which
  the human applies. A µVM runs the binaries already installed, never the ones it just wrote.
- **No CI runs the agent's code with secrets.** The repo has no CI today, and without the
  `workflows` permission the agent can't add any.
- **Its configuration isn't here either.** The instance that works on machinesdancing lives outside
  this repo, like every instance.
- **Review where it matters most.** A subtle change to barre's policy code or jete's network setup
  is the hardest to catch. The first tickets are elsewhere: docs, scripts, tests.

## Step 5: barre G2

Done. The µVM holds no secret: barre sets the Claude token and the GitHub App's token on requests
to the names whose rules give them ([barre.md](design/barre.md#g2-credentials)). The two questions
it left open are answered: a subscription token is added this way like an API key would be, and
git over HTTPS fetches and pushes through it without change.

## Known gaps

Small things that don't block a step, recorded so they're not forgotten. None open now.
