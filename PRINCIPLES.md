# machinesdancing — Principles

## Vision

machinesdancing is a harness for running a team of AI agents as a governed system. A human sets
direction and holds final authority. A planner turns direction into work. Workers execute that
work and deliver it as GitHub pull requests.

It formalizes a working practice that already exists: per-agent clones, a planner as the hub,
tickets and PRs as the record, procedures as shared knowledge. Today that practice is held
together by prose and convention, rules agents are asked to follow. machinesdancing moves those
rules into the system. We trust agents' judgment, not their compliance.

## Principles

1. **Enforced, not requested.** A rule an agent can break by misreading it is only a suggestion.
   Anything that must hold is enforced by the system: the VM, the control layer, the
   gateway. Agent instructions are for judgment, never for safeguards.

2. **Tools, not a monolith.** The harness is built from small, independent tools. Each one does
   one job, is useful on its own, and has a clear interface. Lower tools know nothing about the
   layers above them: the VM manager knows nothing about agents. Higher layers compose them.

3. **Isolation.** Every agent runs in its own slim VM, with its own kernel and workspace. Agents
   run code we did not write, so the boundary is a hypervisor, not a shared kernel. An agent
   reaches only the network, files, secrets, tools and repos its role grants. Nothing is shared
   between agents.

4. **Deny by default.** No permission is implied. Every resource, peer and message type an agent
   can use is granted explicitly by its role.

5. **Strict channels.** Agents communicate only through declared channels. The topology is
   configuration: who may talk to whom, and which message types they may exchange (for example,
   decisions route through the planner while factual questions may go direct). There are no side
   channels.

6. **Control before delivery.** Every message and every external action passes through a control
   layer. Anything that breaks its schema or a business rule is rejected, with a reason the sender
   can act on.

7. **Identity and authority belong to the system.** The system decides who an agent is and what
   it may do. An agent cannot claim another identity, and a peer's message never carries
   authority. A denied action stays denied; it cannot be rerouted through another agent.

8. **Human authority is not delegable.** Designated actions (merges, deploys, production writes)
   need the human's approval, per action. The system issues an approval for one specific action;
   it cannot be relayed, reused or inferred.

9. **The record is the decision.** Durable artifacts on GitHub (tickets, PRs, comments) are the
   truth. Messages are notifications that point at them. A decision or finding that exists only
   in a conversation does not exist.

10. **GitHub-centric.** Work is delivered as pull requests, and an open PR is the claim on that
    work. Agents read others' work from GitHub, never from a stale local copy.

11. **Claims carry evidence.** A message that asserts a fact cites its source: the file and line,
    the query, the measurement, or the instrument used to find it. A claim nobody can check is a
    claim nobody can catch.

12. **Procedures are knowledge.** Recurring tasks are captured as versioned runbooks that agents
    are given by role. When an agent discovers a repeatable workflow, it records the workflow
    instead of leaving the next agent to re-derive it.

13. **The system runs the lifecycle.** Work moves through defined states (queued, in flight,
    review, merged, done). Transitions and their bookkeeping (branches, PRs, ticket status,
    tracker updates) are executed and verified by the system, not left to an agent's memory.

14. **Agents exist on demand.** The orchestrator spawns agents for work and destroys them when it
    is done, taking their access with them. Events such as a merge, a review or a CI result wake
    the right agent. Nobody has to poll.

15. **Declarative.** Roles, channels, schemas, policies, lifecycles and procedures are
    configuration in git, not code inside agents.

16. **Observable.** Every message, decision and lifecycle event, including agent-to-agent
    traffic, is recorded and visible to the human.
