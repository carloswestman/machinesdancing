# Development setup

Code is written and compiled on the Mac and runs on hemlock. Hosts are configured from `ansible/`.

| Machine | Role |
|---|---|
| **Mac** (Apple Silicon) | Editing, compiling, linting, running Ansible. Not managed by Ansible; set up by hand with the steps below. |
| **hemlock** (x86_64, Ubuntu Server) | Runs VMs and the tests that need them. Configured entirely by `ansible/`. Has no compilers. |

## Mac setup

Prerequisites: [Homebrew](https://brew.sh), and SSH access to hemlock with your key (`ssh hemlock`
works without a password; see [`ansible/README.md`](../ansible/README.md)).

1. **Install the tools:**

   ```bash
   brew install rustup ansible gh
   ```

2. **Put rustup on your `PATH`.** Homebrew installs it outside the default path. Add this to
   `~/.zshrc`, then open a new shell:

   ```bash
   export PATH="$(brew --prefix rustup)/bin:$PATH"
   ```

3. **Let the repo pick the toolchain.** Don't install a Rust version by hand. `rust-toolchain.toml`
   at the repo root pins the version, components and the `x86_64-unknown-linux-musl` target, and
   rustup installs exactly those the first time you run `cargo` inside the repo. Check with:

   ```bash
   rustup show
   ```

4. **Install the Ansible collections**, once:

   ```bash
   cd ansible && ansible-galaxy collection install -r requirements.yml
   ```

## How building and testing works

- **Linux by default.** `.cargo/config.toml` sets the build target to `x86_64-unknown-linux-musl`
  and links with `rust-lld`, which ships with Rust. Plain `cargo build`, `cargo clippy` and your
  editor's rust-analyzer all target Linux. jete is Linux-only (KVM, the jailer, vsock, cgroups),
  so building for macOS is neither possible nor useful.
- **Static binaries.** musl links everything in, so a binary runs on hemlock and inside any guest
  image as-is.
- **Tests run on hemlock.** `.cargo/config.toml` sets [`scripts/hemlock-run`](../scripts/hemlock-run)
  as the cargo *runner*. It copies each compiled test or binary to hemlock over SSH, runs it there
  with the same arguments, streams the output back and passes the exit code through. `cargo test`
  and `cargo run` on the Mac therefore execute on hemlock.
- **hemlock needs no GitHub access.** Binaries are copied over SSH. Nothing is cloned there.

The smoke test `crates/jete/tests/host.rs` checks that tests really run on a Linux host with
`/dev/kvm`. If it fails, the runner is not reaching hemlock.

Tests that boot VMs need root and a current guest image, so they are marked `#[ignore]` and run
on request:

```bash
scripts/build-rootfs                                          # after changing jete-agent or images/
JETE_RUN_SUDO=1 cargo test -- --ignored --nocapture           # boots jailed VMs; prints time-to-ready
```

The network tests (`crates/jete/tests/net.rs`) also need IPv4 forwarding on the host (the
`network` Ansible role) and internet access from it: a networked VM reaches `1.1.1.1`. barre's
tests (`crates/barre/tests/vm.rs`) need the same, and `curl` in the guest image; they run barre
inside the test, on its own port and policy directory, so an installed barre is not disturbed.

### Running `jete`

On hemlock, `jete` is installed as `/usr/local/bin/jete` and runs as root:

```bash
id=$(sudo jete run)              # boots a VM; prints its id, and "ready in N ms" on stderr
sudo jete ls
sudo jete exec $id -- uname -a   # stdin, stdout, stderr and the exit code pass through
sudo jete shell $id              # interactive; exit the shell to leave
sudo jete rm $id
```

To install or update it, build a release on the Mac and copy it over with Ansible:

```bash
scripts/install-jete --check --diff   # dry run
scripts/install-jete                  # build, then install on the VM hosts
```

From the Mac, run your working copy on hemlock through the runner instead, without installing it:

```bash
alias jete='JETE_RUN_SUDO=1 cargo run -q --bin jete --'   # in the repo; add to your shell if you like
jete ls
```

### Running barre

barre, the egress gateway ([barre.md](barre.md)), runs on hemlock as the `barre` service, installed
with `scripts/install-barre`. That also installs `barre-vm` in `/usr/local/bin`, a short script
(source: `ansible/roles/barre/templates/barre-vm.j2`) that boots a VM behind the service with a
policy, and removes both:

```bash
id=$(sudo barre-vm run example.com github.com:22)   # jete run --egress-proxy 15001, then barre allow
sudo jete exec $id -- curl -sI https://example.com
sudo barre ls                                       # policies: address, VM, rules
journalctl -u barre -o cat -f                       # every query and connection, with its decision
sudo barre-vm rm $id                                # barre revoke, then jete rm
```

From the Mac, run your working copy on hemlock through the runner instead, as with `jete`:

```bash
alias barre='JETE_RUN_SUDO=1 cargo run -q --bin barre --'   # in the repo; add to your shell if you like
barre ls
```

`ls`, `allow` and `revoke` act on the service's policies, which it picks up within half a second.
`serve` needs its own port and policy directory, since the service holds port 15001; start VMs
for it with that port and give it their policies by hand, as `barre-vm` would:

```bash
ssh hemlock sudo mkdir -p /run/barre-dev
barre --policies /run/barre-dev serve --port 15002   # in one terminal; Ctrl-C stops it

# in another:
id=$(jete run --egress-proxy 15002)
addr=$(ssh hemlock ip -4 -br addr show jete-$id | awk '{print $3}'); addr=${addr%/*}
barre --policies /run/barre-dev allow $addr --vm $id example.com
jete exec $id -- curl -sI https://example.com
jete rm $id
```

### Runner settings

| Variable | Default | Effect |
|---|---|---|
| `JETE_HOST` | `hemlock` | SSH host the runner uses. |
| `JETE_RUN_SUDO` | unset | Set to `1` to run the binary under `sudo` on the host, for code that needs root (the jailer). Runs unattended: the admin user has passwordless `sudo` on hemlock. |
| `RUST_BACKTRACE`, `RUST_LOG`, `RUST_TEST_THREADS` | | Forwarded to the host when set. |

The runner keeps one SSH connection open for 60 seconds, so a test run with many binaries does not
reconnect for each one. Uploads are gzip-compressed and named by content hash under
`~/.cache/jete-run` on hemlock, and kept for a day: an unchanged binary is not sent again, and
concurrent runs never overwrite each other's copies. Debug builds keep only line tables
(`[profile.dev]` in `Cargo.toml`) to stay small. An interactive terminal is passed through when you
have one, which `jete shell` needs.

## Everyday commands

From the repo root:

```bash
cargo build                 # static Linux binaries in target/x86_64-unknown-linux-musl/
cargo run --bin jete        # build on the Mac, run on hemlock
cargo clippy                # lint
cargo fmt                   # format
cargo test                  # compile on the Mac, run on hemlock
```

Building the guest root filesystem, with a fresh release build of `jete-agent` inside (on hemlock,
about 40 seconds; prints the new image's path):

```bash
scripts/build-rootfs
```

Installing `jete` on hemlock (release build, through `ansible/jete.yml`):

```bash
scripts/install-jete
```

Installing barre, the egress gateway, on hemlock as a service, with `barre-vm` (release build,
through `ansible/barre.yml`):

```bash
scripts/install-barre
```

Changing hemlock itself:

```bash
cd ansible
ansible-playbook site.yml --check --diff   # dry run
ansible-playbook site.yml                  # apply
```

## Conventions

- Work on a branch and open a pull request. Nothing goes to `main` directly.
- Hosts change only through `ansible/`. Never edit configuration on hemlock by hand.
- Secrets never live in the repo. See *Environment variables* in [`ansible/README.md`](../ansible/README.md).
