# Orchestration Patterns

Reference catalog of agent orchestration patterns this repo endorses, plus anti-patterns to avoid. Read this before adding a new persona or slash command that coordinates multiple reviewers, or before introducing a new persona that "wraps" existing ones.

zynk's own development gates (Codex collaborative review, swarm independent verification, the operator merge gate) are the canonical worked example of these patterns — see `WORKFLOW.md`. The governing rule mirrors that contract: **the operator (or a slash command) is the orchestrator. Personas do not invoke other personas.** Skills are mandatory hops inside a persona's workflow.

---

## Endorsed patterns

### 1. Direct invocation (no orchestration)

Single persona, single perspective, single artifact. The default and the cheapest option.

```
operator → code-reviewer → report → operator
```

**Use when:** the work is one perspective on one artifact and you can describe it in one sentence.

**Examples:**
- "Review this diff" → `code-reviewer`
- "Find local-socket threat-model gaps in `src/ipc.rs`" → `security-auditor`
- "What characterization tests are missing for the protocol-ID fields?" → `test-engineer`

**Cost:** one round trip. The baseline you should always compare orchestrated patterns against.

---

### 2. Single-persona slash command

A slash command that wraps one persona with the project's skills. Saves the operator from re-explaining the workflow every time.

```
/review → code-reviewer (with code-review-and-quality skill) → report
```

**Use when:** the same single-persona invocation happens repeatedly with the same setup.

**Examples in this repo:** `/review`, `/test`, `/code-simplify`.

**Cost:** same as direct invocation. The slash command is just a saved prompt.

**Anti-signal:** if the slash command's body is mostly "decide which persona to call," delete it and let the operator call the persona directly.

---

### 3. Parallel fan-out with merge (the swarm)

Multiple personas operate on the same input concurrently, each producing an independent report. A merge step (in the main agent's context, or in an arbiter) synthesizes them into a single decision. This is exactly zynk's **Gate-3 swarm**: an arbiter fans out decorrelated specialist reviewers (correctness, security, regression, does-it-reproduce), cross-verifies their findings, and reports one verdict through the audited zynk conversation.

```
                    ┌─→ code-reviewer    ─┐
swarm → fan out  ───┼─→ security-auditor ─┤→ merge → approve / request-changes
                    └─→ test-engineer    ─┘
```

**Use when:**
- The sub-tasks are genuinely independent (no shared mutable state, no ordering dependency)
- Each sub-agent benefits from its own context window
- The merge step is small enough to stay in the main context
- Wall-clock latency matters

**Examples in this repo:** the Gate-3 swarm verification described in `WORKFLOW.md` (global `swarm` skill).

**Cost:** N parallel sub-agent contexts + one merge turn. Higher than direct invocation, but faster wall-clock and produces better reports because each sub-agent stays focused on its single perspective.

**Validation checklist before adopting this pattern:**
- [ ] Can I run all sub-agents at the same time without ordering issues?
- [ ] Does each persona produce a different *kind* of finding, not just the same finding from a different angle?
- [ ] Will the merge step fit in the main agent's remaining context?
- [ ] Is the operator's wait time long enough that parallelism is actually noticeable?

If any answer is "no," fall back to direct invocation or a single-persona command.

---

### 4. Sequential pipeline as user-driven gates

The operator drives a defined sequence of gates, carrying context (a spec, a diff, or commit history) between them. There is no orchestrator agent — the operator IS the orchestrator. This is zynk's full development flow: `WORKFLOW.md`'s Gate-1 (Codex reviews the spec) → Gate-2 (Codex reviews the implementation) → Gate-3 (swarm independent verification) → operator merge approval.

```
operator drives:  spec  →  Gate-1  →  implement  →  Gate-2  →  Gate-3  →  merge
```

**Use when:** the workflow has dependencies (each step needs the previous step's output) and human judgment between steps adds value.

**Examples in this repo:** the entire `WORKFLOW.md` gate sequence (spec → Gate-1 → implement → Gate-2 → Gate-3 → operator merge).

**Cost:** one sub-agent context per step. Free for the orchestration layer because there is no orchestrator agent.

**Why not automate it:** an LLM "lifecycle orchestrator" would (a) lose nuance between gates because it has to summarize for hand-off, (b) skip the operator checkpoints that catch wrong-direction work early (and the explicit per-action merge/push gate `WORKFLOW.md` requires), and (c) double the token cost via paraphrasing turns.

---

### 5. Research isolation (context preservation)

When a task requires reading large amounts of material that shouldn't pollute the main context, spawn a research sub-agent that returns only a digest.

```
main agent → research sub-agent (reads 50 files) → digest → main agent continues
```

**Use when:**
- The main session needs to stay focused on a downstream task
- The investigation result is much smaller than the input it consumes
- The decision quality benefits from the main agent having room to think after

**Examples:** "Find every call site of `compute_view()` across `src/`," "Summarize what these ADRs in `docs/zynk/decisions/` say about the fail-closed DB."

**Cost:** one isolated sub-agent context. Worth it any time the alternative is loading hundreds of files into the main context.

**On Claude Code, use the built-in `Explore` subagent** rather than defining a custom research persona. `Explore` runs on a cheaper model, is denied write/edit tools, and is purpose-built for this pattern. Define a custom research subagent only when `Explore` doesn't fit (e.g. you need a domain-specific system prompt the model wouldn't infer).

---

## Claude Code compatibility

This catalog is harness-agnostic, but most readers will run it on Claude Code. Here's how each pattern maps onto Claude Code's primitives — and where the platform enforces our rules for us.

### Where personas live

Plugin subagents go in `agents/` at the plugin root; project personas go in `.claude/agents/`. The personas this catalog references (`code-reviewer`, `security-auditor`, `test-engineer`) are auto-discovered when present. No path configuration needed.

### Subagents vs. Agent Teams

Claude Code has two parallelism primitives. Pattern 3 (parallel fan-out with merge) maps to **subagents**. If you need teammates that talk to each other, use **Agent Teams** instead.

| | Subagents | Agent Teams |
|--|-----------|-------------|
| Coordination | Main agent fans out, sub-agents only report back | Teammates message each other, share a task list |
| Context | Own context window per subagent | Own context window per teammate |
| When to use | Independent tasks producing reports | Collaborative work needing discussion |
| Status | Stable | Experimental — requires `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` |
| Cost | Lower | Higher — each teammate is a separate instance |

**The personas in this repo work in both modes.** When spawned as subagents (e.g. by the swarm), they report findings to the main session. When spawned as teammates, they can challenge each other's findings directly. The persona definition is the same; only the spawning context changes.

One subtlety: the `skills` and `mcpServers` frontmatter fields in a persona are honored when it runs as a subagent but **ignored when it runs as a teammate** — teammates load skills and MCP servers from your project and user settings, the same as a regular session. If a persona depends on a specific skill or MCP server being loaded, configure it at the session level so it's available in both modes.

### zynk's native coordination layer

Separate from Claude Code's in-harness primitives, zynk's own development model coordinates **live peer agents** (Codex, Pi) in adjacent panes over the audited Unix-socket transport — not in-harness subagents. Hand-offs use `zynk send <pane> --type request-review|request-changes|approve --trace <id> -- "<text>"` then `zynk reply`; the authoritative verdict is the audited conversation (`zynk thread` / `zynk trace <id>`), not the `delivery_status`. This is the real-world embodiment of Pattern 4 (sequential gates) and Pattern 3 (the swarm), with the transport providing the audit trail. See `WORKFLOW.md` §"Message bodies (native zynk)".

### Platform-enforced rules

Two rules in this catalog aren't just convention — Claude Code enforces them:

- **"Subagents cannot spawn other subagents."** Anti-pattern B (persona-calls-persona) and Anti-pattern D (deep persona trees) cannot exist on Claude Code by construction.
- **"No nested teams"** — teammates cannot spawn their own teams. Same anti-patterns blocked at the team level.

This means you can adopt the patterns in this catalog without worrying about contributors accidentally building the anti-patterns. They'll just fail to load.

### Built-in subagents to know about

Before defining a custom subagent, check whether one of these covers the role:

| Built-in | Purpose |
|----------|---------|
| `Explore` | Read-only codebase search and analysis. Use this for Pattern 5 (research isolation). |
| `Plan` | Read-only research during plan mode. |
| `general-purpose` | Multi-step tasks needing both exploration and modification. |

Don't redefine these. Layer your specialist personas (code-reviewer, security-auditor, test-engineer) on top of them.

### Spawning multiple subagents in parallel

In Claude Code, parallel fan-out (Pattern 3) requires issuing **multiple Agent tool calls in a single assistant turn**. Sequential turns serialize execution. Any new orchestrator command (or a swarm arbiter) should do the same.

---

## Worked example: Agent Teams for competing-hypothesis debugging

This example shows when to reach for **Agent Teams** instead of the swarm's subagent fan-out. The two patterns look similar from a distance — both spawn the same three personas — but the value comes from a different place.

### The scenario

> *An agent pane intermittently hangs for ~30 seconds before a `zynk send` returns. It happens roughly once every 50 hand-offs. No errors in logs. Started after last week's change to the IPC frame reader.*

Plausible root causes (mutually exclusive, all fit the symptoms):

1. A lock held across an `.await` in the server's per-client write path (tokio task starvation)
2. A `SQLITE_BUSY` retry loop in the delivery-event write that occasionally backs off too long
3. A blocking call (e.g. a synchronous `spawn`/git probe) accidentally on the async executor thread
4. A frame-size guard edge case where an oversized payload triggers a slow re-read path

A single agent will pick the first plausible theory and stop investigating. A swarm-style subagent fan-out would have each persona report independently — but their reports never meet, so nothing rules out the wrong theories.

This is exactly the case the Agent Teams docs describe: *"With multiple independent investigators actively trying to disprove each other, the theory that survives is much more likely to be the actual root cause."*

### Why this is *not* a swarm job

| | Swarm (subagents) | Agent Teams |
|--|--------------------|-------------|
| Sub-agents see | The same diff, different lenses | A shared task list, each other's messages |
| Output | Three independent reports → one merge | Adversarial debate → consensus root cause |
| Right when | You want a verdict on a known artifact | You want to *find* the artifact among hypotheses |

The swarm is a verdict; Agent Teams is an investigation.

### Setup (one-time, per-environment)

Agent Teams is experimental. In `~/.claude/settings.json`:

```json
{
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  }
}
```

The personas in this repo are picked up automatically — no team-config files to author by hand.

### The trigger prompt

Type into the lead session, in natural language:

```
A zynk send intermittently hangs for ~30 seconds after last week's IPC
frame-reader change. No errors in logs.

Create an agent team to debug this with competing hypotheses. Spawn
three teammates using the existing agent types:

  - code-reviewer  — investigate lock-across-await and blocking calls
                     on the tokio executor in the server write path
  - security-auditor — investigate the frame-size guard and any
                       allocation/back-off path on oversized payloads
  - test-engineer  — propose tests that would distinguish between the
                     hypotheses and check coverage gaps in the IPC layer

Have them message each other directly to challenge each other's
theories. Update findings as consensus emerges. Only converge when
two teammates agree they can disprove the others'.
```

The lead spawns three teammates referencing the existing persona names. The persona body is **appended** to each teammate's system prompt as additional instructions (on top of the team-coordination instructions the lead installs); the trigger prompt above becomes their task.

### What happens

1. Each teammate runs in its own context window, exploring the codebase from its own lens.
2. Teammates use `message` to send findings to each other directly. The lead doesn't have to relay.
3. The shared task list shows who's investigating what — visible at any time.
4. When `code-reviewer` finds a `MutexGuard` held across an `.await` in the write path, it messages `security-auditor` to confirm the frame-size guard isn't the real culprit. `security-auditor` checks and replies — either confirming the lock is the real issue or producing counter-evidence.
5. `test-engineer` proposes a focused `#[tokio::test]` for whichever theory is winning, which the team uses to verify before declaring consensus.
6. The lead synthesizes the converged finding and presents it.

You can interrupt at any teammate and redirect an investigator who's gone down a wrong path.

### When to clean up

When the investigation lands on a root cause, tell the lead:

```
Clean up the team
```

Always cleanup through the lead, not a teammate (per the docs: teammates lack full team context for cleanup).

### Cost expectation

Three teammates running for ~10–15 minutes of investigation costs noticeably more than the same three personas spawned as swarm subagents. The justification is *quality of conclusion* — for a production-bound concurrency bug where the wrong fix is expensive, the extra tokens are a bargain. For a routine diff review, stick with the swarm.

### Anti-pattern in this scenario

Do **not** rebuild this as a `/debug` slash command that fans out subagents. Subagents can't message each other — you'd lose the adversarial debate that makes the pattern work. If a workflow keeps coming up, document the trigger prompt above as a snippet rather than wrapping it in a slash command that misuses subagents.

### When *not* to use Agent Teams

- Production-bound verdict on a known diff → use the swarm (subagents).
- One specialist perspective on one artifact → direct persona invocation.
- Sequential gate flow (spec → Gate-1 → implement → Gate-2) → user-driven gates (Pattern 4).
- Read-heavy research with a small digest → built-in `Explore` subagent.

Reach for Agent Teams only when teammates **need** to challenge each other to produce the right answer.

---

## Anti-patterns

### A. Router persona ("meta-orchestrator")

A persona whose job is to decide which other persona to call.

```
/work → router-persona → "this needs a review" → code-reviewer → router (paraphrases) → operator
```

**Why it fails:**
- Pure routing layer with no domain value
- Adds two paraphrasing hops → information loss + roughly 2× token cost
- The operator already knew they wanted a review; they could have called `/review` directly
- Replicates the work that slash commands and intent mapping in `AGENTS.md` already do

**What to do instead:** add or refine slash commands. Document intent → command mapping in `AGENTS.md`.

---

### B. Persona that calls another persona

A `code-reviewer` that internally invokes `security-auditor` when it sees socket-binding code.

**Why it fails:**
- Personas were designed to produce a single perspective; chaining them defeats that
- The summary the calling persona passes loses context the called persona needs
- Failure modes multiply (which persona's output format wins? whose rules apply?)
- Hides cost from the operator

**What to do instead:** have the calling persona *recommend* a follow-up audit in its report. The operator or the swarm runs the second pass.

---

### C. Sequential orchestrator that paraphrases

An agent that runs the spec, then the implementation, then the gates on the operator's behalf.

**Why it fails:**
- Loses the operator checkpoints that catch wrong-direction work (and the per-action merge/push gate `WORKFLOW.md` mandates)
- Each hand-off summarizes context — accumulated drift over a long pipeline
- Doubles token cost: orchestrator turn + sub-agent turn for every step
- Removes operator agency at exactly the points where judgment matters most

**What to do instead:** keep the operator as the orchestrator. Document the recommended sequence in `WORKFLOW.md` and let the operator drive it.

---

### D. Deep persona trees

A swarm arbiter that calls a `pre-merge-coordinator` that calls a `quality-coordinator` that calls `code-reviewer`.

**Why it fails:**
- Each layer adds latency and tokens with no decision value
- Debugging becomes a multi-level investigation
- The leaf personas lose context to multiple summarization steps

**What to do instead:** keep the orchestration depth at most 1 (slash command / arbiter → personas). The merge happens in the main agent.

---

## Decision flow

When considering a new orchestrated workflow, walk this flow:

```
Is the work one perspective on one artifact?
├── Yes → Direct invocation. Stop.
└── No  → Will the same composition repeat?
         ├── No  → Direct invocation, ad hoc. Stop.
         └── Yes → Are sub-tasks independent?
                  ├── No  → Sequential gates driven by the operator (Pattern 4).
                  └── Yes → Parallel fan-out with merge / swarm (Pattern 3).
                           Validate against the checklist above.
                           If any check fails → fall back to single-persona command (Pattern 2).
```

---

## When to add a new pattern to this catalog

Add a new entry only after:

1. You've used the pattern at least twice in real work
2. You can name a concrete artifact in this repo that demonstrates it
3. You can explain why an existing pattern wouldn't have worked
4. You can describe its anti-pattern shadow (what people will mistakenly build instead)

Premature catalog entries become aspirational documentation that no one follows.
