# machinesdancing

A harness for running a team of AI agents as a governed system, built from small, independent
tools.

| Start here | |
|---|---|
| [PRINCIPLES.md](PRINCIPLES.md) | Vision and principles. Every design decision must comply with them. |
| [docs/architecture.md](docs/architecture.md) | The big picture: the tools, how they connect, and the build order. |
| [docs/design/jete.md](docs/design/jete.md) | Design of jete, the first tool: a fast Firecracker VM manager. |
| [docs/design/barre.md](docs/design/barre.md) | Design of barre, the egress gateway: what VMs may reach, by name. |
| [docs/guides/development.md](docs/guides/development.md) | Setting up the Mac, and how building and testing work. |
| [docs/guides/claude-code.md](docs/guides/claude-code.md) | Running Claude Code in a VM behind barre, on your Claude subscription. |
| [ansible/](ansible/) | Configuration of the hosts that run everything (currently hemlock). |
