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
- **Tests run on hemlock.** `.cargo/config.toml` sets a cargo *runner*: a script in the repo that
  copies each compiled test or binary to hemlock over SSH and runs it there, streaming the output
  back. `cargo test` and `cargo run` on the Mac therefore execute on hemlock.
- **hemlock needs no GitHub access.** Binaries are copied over SSH. Nothing is cloned there.

> The Cargo workspace, `rust-toolchain.toml`, `.cargo/config.toml` and the runner script arrive with
> the first M1 pull request. Until then, steps 1, 2 and 4 are all that apply.

## Everyday commands

From the repo root, once the workspace exists:

```bash
cargo build                 # static Linux binaries in target/x86_64-unknown-linux-musl/
cargo clippy                # lint
cargo fmt                   # format
cargo test                  # compile on the Mac, run on hemlock
```

Changing hemlock itself:

```bash
cd ansible
ansible-playbook site.yml -K --check --diff   # dry run
ansible-playbook site.yml -K                  # apply
```

## Conventions

- Work on a branch and open a pull request. Nothing goes to `main` directly.
- Hosts change only through `ansible/`. Never edit configuration on hemlock by hand.
- Secrets never live in the repo. See *Environment variables* in [`ansible/README.md`](../ansible/README.md).
