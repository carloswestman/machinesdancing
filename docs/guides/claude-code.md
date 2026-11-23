# Running Claude Code in a VM

Claude Code runs in a jete VM on hemlock, behind barre, on your Claude subscription. The VM may
reach `api.anthropic.com` and whatever else you allow, by name, and nothing else. Every
connection it makes, or is refused, is in barre's log.

This is steps 3 and 4 of the build order ([build-order.md](../build-order.md)): one agent,
started by hand, working on a repo.

## What you need

- barre installed on hemlock (`scripts/install-barre`; see [development.md](development.md#running-barre)).
- A guest image with Claude Code. `scripts/build-rootfs` adds it: the stable release, or the one
  `CLAUDE_CODE_VERSION` names (`stable`, `latest` or a version number), checked against its release
  manifest. It runs as the unprivileged user `agent`, and doesn't update itself.
- A token for your subscription, kept on the Mac. Once:

```bash
claude setup-token        # signs in with your subscription; prints a long-lived token. Copy it.
mkdir -p ~/.config/machinesdancing
pbpaste > ~/.config/machinesdancing/claude-token
chmod 600 ~/.config/machinesdancing/claude-token
```

## Running a prompt

From the repo, on the Mac:

```bash
scripts/claude-vm "what is in your home directory?"   # boots a VM; prints its id last
scripts/claude-vm --vm ID "and now?"                  # the same VM again: files it wrote are still there
```

`claude-vm` boots the VM with `barre-vm run api.anthropic.com`, then runs
`claude -p --dangerously-skip-permissions PROMPT` in it as `agent`. There are no permission
prompts: the VM and barre are what contain it, not Claude Code's own checks.

The token goes over ssh's stdin into Claude Code's environment. It is never on a command line, and
never written to hemlock or to the VM's disk. It is, though, inside the VM while Claude Code runs,
so anything in the VM could read it and use it, toward the names the VM may reach. Keeping
credentials out of VMs entirely is barre's phase 2 ([barre.md](../design/barre.md#phase-2-credentials)).

## Letting it reach more

`CLAUDE_VM_ALLOW` adds barre rules, space-separated, when the VM boots:

```bash
CLAUDE_VM_ALLOW="github.com:22,443 *.githubusercontent.com" scripts/claude-vm "clone ... and ..."
```

To change what a running VM may reach, replace its whole policy on hemlock. `barre ls` shows each
VM's host-side address, which `allow` takes:

```bash
sudo barre ls
sudo barre allow 10.200.0.5 --vm ID api.anthropic.com github.com:22,443
```

Rules are `NAME`, `*.NAME` or `ADDRESS[/PREFIX]`, then optionally `:PORT[,PORT...]`; port 443 when
none is given ([barre.md](../design/barre.md#policies)).

## Working on a repo

With an instance ([instances.md](../design/instances.md)), the agent works on one of its repos with
`git` and `gh`, as the role's GitHub App, and delivers a pull request
([repo-access.md](../design/repo-access.md)). Once:

1. Create the role's App: `scripts/create-github-app NAME`, then install it on the repos the role
   works on, and only those.
2. Put its key on hemlock: apply `site.yml` (the `credentials` role), then
   `scripts/install-github-app NAME`. Check with
   `ssh hemlock 'sudo github-token NAME OWNER/REPO >/dev/null && echo ok'`.
3. Protect the repo's `main` with a ruleset: restrict updates, with only Repository admin (you)
   allowed to bypass, for pull requests; changes only by pull request, with a code owner's review;
   you the only code owner. Restricting updates is what keeps merging yours: the App's permissions
   let it approve and merge ([why](../design/repo-access.md#why-restricting-updates-not-only-reviews)).
4. Make the instance: a directory, best a private repo of its own, outside this one, with an
   `instance.toml`:

```toml
name = "mine"

[roles.worker]
github_app = "NAME"
allow = ["api.anthropic.com", "github.com", "api.github.com", "*.githubusercontent.com"]

[[repos]]
url = "https://github.com/OWNER/REPO"
roles = ["worker"]
```

Then:

```bash
export MD_INSTANCE=~/path/to/instance
scripts/claude-vm "Fix the typo in README.md's first paragraph."
scripts/claude-vm --role worker --repo OWNER/REPO "..."    # when there is more than one
```

`claude-vm` boots the VM with the role's `allow` rules, mints the App's token on hemlock and writes
it into the VM there (it never reaches the Mac), and tells Claude Code which repo it works on and
that its work goes in a pull request. The token lasts an hour; `claude-vm` replaces it every 50
minutes while Claude Code runs. Commits are authored by the App's bot, as `ROLE (µVM ID)`.

## Seeing what it did

```bash
ssh hemlock journalctl -u barre -o cat | grep ID       # each lookup and connection, allowed or refused, with the reason
ssh hemlock sudo jete exec ID -- ls -la /home/agent    # what it left behind
```

A refused connection looks to Claude Code like a network error: a name that doesn't resolve, or a
connection closed before TLS completes. The reason is only in barre's log. So when a task fails
for no clear reason, check the log for refusals, and allow what it legitimately needs.

## Cleaning up

VMs stay running until removed:

```bash
ssh hemlock sudo jete ls
ssh hemlock sudo barre-vm rm ID      # removes its policy, then the VM
```

## Limits, for now

- **Memory.** VMs get jete's default, 512 MiB, which is enough for short prompts. `barre-vm` has no
  way to ask for more yet.
- **One prompt at a time, by hand.** No tickets, no bus, no control plane: those come later in
  the [build order](../build-order.md).
- **Subscription terms.** Your own agents, on your own machines, for your own work. Before VMs run
  work for anyone else, this moves to an API key added by barre.
