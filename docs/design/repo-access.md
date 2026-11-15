# Repo access — design

How an agent works on its instance's repos: it uses `git` and `gh` like a developer on the team,
as an identity the host chooses, with limits enforced by GitHub, barre and the host rather than by
its instructions. Step 4 of the [build order](../build-order.md). Which repos, and which roles, is
the instance's configuration ([instances.md](instances.md)).

## Scope

**Does**

- Give an agent a clone of a repo its role may work on, with push access to branches.
- Let it open pull requests and comment on them.
- Make what it pushes attributable: to its role by GitHub, to its µVM and ticket by the commit
  author.

**Does not**

- Wrap git in a tool of our own. The agent knows git; a wrapper would cost tokens and add nothing.
- Let an agent merge into `main`. Only the human does (principle 8).
- Teach barre about git or GitHub's API. barre allows names and ports and, from G2, adds
  credentials. It doesn't read requests.

## Trust model

Git's own: everyone with write access is trusted to push branches and open pull requests, and the
guardrail is the merge into `main`. An agent is trusted like a developer on the team, and no more.

| Limit | Enforced by |
|---|---|
| Which repos | The GitHub App is installed on the instance's repos only. The host narrows each token further, to the one repo the task is on. |
| What it may do there | The App's permissions: contents and pull requests, read and write. No administration and no `workflows`, so GitHub refuses a push that changes `.github/workflows/`, and the agent can't add CI that runs with the repo's secrets. |
| No merging into `main` | A ruleset or branch protection on `main`: changes arrive only by pull request, with a code owner's approval, and the human is the only code owner. An agent's approval never counts. |
| Where it can reach | barre: the role's `allow` rules in the instance include `github.com` and `api.github.com`. |
| Who pushed | The token. The host mints it for the µVM's role; the agent doesn't choose it and holds no other. |
| Which µVM wrote a commit | The commit author, set by the host (see [Identity](#identity)). |

An identity can push to any unprotected branch, including another agent's. That's how git works on
a team too, and it's left to review: the human sees every commit before a merge, and each commit
names its ticket. When agents run at the same time, a regisseur rule can flag a pull request whose
branch has commits from another ticket.

## Identity

- **One GitHub App per role.** Each App is its own bot user (`example-worker[bot]`), with its own
  permissions: a worker opens pull requests, a planner might only comment. Step 4 needs one role,
  `worker`.
- **Not one per µVM.** GitHub identities are permanent and µVMs are thrown away after each task.
  Within a role, the commit author tells µVMs apart.
- **The author is set by the host, not asked of the agent.** The environment the agent starts in
  sets `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL`, for
  example `worker (µVM 39195192)` with the App's bot email. The agent could override them, but only
  deliberately, and review would see it.
- **Not machine users.** GitHub's terms allow one free machine account per person; Apps scale to
  several roles, and their tokens are short-lived.

## How a task gets access

1. The host reads the instance: the role, its App, its barre rules, the repo.
2. It starts the µVM with the role's barre rules.
3. It mints an installation token for the role's App, narrowed to the task's repo, valid for an
   hour, from the App's private key in `/etc/machinesdancing/credentials/` (the key never leaves the
   host).
4. It starts the agent with the token on stdin, as `claude-vm` does the Claude token: never on a
   command line or a disk. The agent's environment gets it as `GH_TOKEN`, which `gh` reads, and git
   uses it through a credential helper in the guest image that reads the same variable.
5. The agent clones, branches, commits, pushes and opens its pull request.

From G2 the µVM holds no token: barre adds it to requests to `github.com` and `api.github.com`.

## Known limits

- **Until G2, the µVM holds a token.** It's narrowed to one repo and expires within an hour.
- **Agents in one role share an identity.** GitHub can't tell whether worker A pushed to ticket B's
  branch; only the commit author says so, and the agent can change it. Review covers this for now.

## Later: signed commits

If review isn't enough once agents run at the same time, commits can be signed by the host:

- Git signs each commit itself, through the signing program it's configured with
  (`gpg.ssh.program`): a small helper in the guest image. The agent isn't asked, and no MCP tool is involved.
- The helper asks a signer on the host over vsock, on mime's port. The host knows which µVM is
  asking and signs with that µVM's key, which never enters the µVM. (jete-agent's channel only
  runs from the host into the guest, so it isn't the place.)
- GitHub won't show these commits as verified, since an App can't register keys. The check is a
  regisseur rule: each commit on a ticket's branch is signed by that ticket's µVM.

## Milestone

**Done when** a ticket on the instance's repo, started by hand, ends in a pull request opened by the
worker App, with commits authored by its µVM, which the human reviews and merges; and, from inside
that µVM, pushing to `main`, approving the pull request and pushing a change under
`.github/workflows/` are each refused by GitHub.

## Open questions

1. **What mints the token.** A script on the host, or a small Rust tool. Either way it runs as
   root, since it reads the App's key.
2. **The guest's git setup.** The credential helper, `gh`, and the bot email are baked into the
   guest image, or written into the µVM when the task starts.
