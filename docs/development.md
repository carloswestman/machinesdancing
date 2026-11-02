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
  with the same arguments, streams the output back, passes the exit code through, and deletes the
  binary. `cargo test` and `cargo run` on the Mac therefore execute on hemlock.
- **hemlock needs no GitHub access.** Binaries are copied over SSH. Nothing is cloned there.

The smoke test `crates/jete/tests/host.rs` checks that tests really run on a Linux host with
`/dev/kvm`. If it fails, the runner is not reaching hemlock.

Tests that boot VMs need root and a current guest image, so they are marked `#[ignore]` and run
on request:

```bash
scripts/build-rootfs                                          # after changing jete-agent or images/
JETE_RUN_SUDO=1 cargo test -- --ignored --nocapture           # boots a VM; prints time-to-ready
```

### Runner settings

| Variable | Default | Effect |
|---|---|---|
| `JETE_HOST` | `hemlock` | SSH host the runner uses. |
| `JETE_RUN_SUDO` | unset | Set to `1` to run the binary under `sudo` on the host, for code that needs root (the jailer). Runs unattended: the admin user has passwordless `sudo` on hemlock. |
| `RUST_BACKTRACE`, `RUST_LOG`, `RUST_TEST_THREADS` | | Forwarded to the host when set. |

The runner keeps one SSH connection open for 60 seconds, so a test run with many binaries does not
reconnect for each one. An interactive terminal is passed through when you have one, so
`cargo run --bin jete -- shell <id>` will work once it exists.

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
