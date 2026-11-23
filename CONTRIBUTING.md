# Contributing

machinesdancing is experimental and built in the open (see the [README](README.md)). It is
licensed under [Apache 2.0](LICENSE).

## Before you start

Read [PRINCIPLES.md](PRINCIPLES.md). Every design decision must comply with it. The design docs
live in [docs/design/](docs/design/), with the big picture in
[docs/architecture.md](docs/architecture.md) and what comes next in
[docs/build-order.md](docs/build-order.md). If a change touches a design, read its doc first.

## Setting up and building

[docs/guides/development.md](docs/guides/development.md) covers it: installing the tools,
how the pinned toolchain builds static Linux binaries, and how tests run on the Linux host
over SSH. The README's [Try it](README.md#try-it) section walks through an end-to-end run.

## Making a change

- Work on a branch and open a pull request. Nothing goes to `main` directly.
- Every change to `main` needs review by the code owner, listed in
  [.github/CODEOWNERS](.github/CODEOWNERS).
- Hosts change only through `ansible/`, never by hand.
- Secrets never live in the repo.

These are the conventions in [docs/guides/development.md](docs/guides/development.md#conventions).

## Security

To report a vulnerability, do not open a public issue. See [SECURITY.md](SECURITY.md).
