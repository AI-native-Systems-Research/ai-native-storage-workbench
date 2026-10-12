# The FV agent - verify one component, unattended, honestly

## What it is for
Given a Certus component (its spec and its code), the FV agent finds out which of the component's promises
the code keeps, with machine-checked proofs (Creusot and Kani). It also reports, in plain English, where spec
and code disagree. It is built so that nobody has to trust what an AI says: an AI does the work, and fixed code
decides what counts.

## The steps (run by code, in this order)
```
extract  ->  sync  ->  classify  ->  prove (Creusot)  ->  prove (Kani)  ->  check  ->  supervisor (<= 2 rounds)  ->  publish
```
| step | what happens | who decides |
|---|---|---|
| extract | Two **blind readers**: one sees only the spec, the other only the code. Each lists the component's verifiable properties. A third agent pairs the two lists (same / different / spec-only / code-only); the driver builds the merged list from that pairing. | the driver checks every input id is used exactly once |
| sync | An agent re-checks each "spec and code agree" pair against the code. It also finds **code assumptions** (e.g. "the message contains no newline"). | the driver validates the report and applies it |
| classify | Two independent agents sort every disagreement into **A** prove it, **B** a bug for the repair agent, **C** another tool. When they disagree, the case goes to a person. | the merge rule |
| prove | One agent per tool writes proofs, each with a **mutant twin** that must fail. Proofs can also be **refutations**: a proof that the code breaks the property. | the **scorer**, by re-running every proof itself |
| check | The definition of done: every candidate classified, every property scored, no proof older than a code assumption it depends on, no contradiction between the two tools. | `check_done.py` |
| supervisor | If not done: ONE judgement call, choosing from a fixed menu - retry named properties for one tool, or stop with a reason a person can act on. A retry that closed nothing cannot be repeated. | Python limits the menu and the rounds |
| publish | Two LOCAL branches (`verif/creusot/<c>-<date>`, `verif/kani/<c>-<date>`) holding the property lists, the pages and that tool's proofs. | never pushes |

## Safety rails (all enforced by code, not by asking)
- **Agents never write a status.** Only the scorers write `proved` / `refuted` / `delegated`.
- **Hooks on every agent call:** writes only to that step's allowed paths, hidden files stay hidden, no
  git commit/push/reset, no `sudo`, no background shells, at most 2 prover processes at a time. A stop check
  makes the agent finish its step's work before it may stop.
- **The runtime** (`fv_runtime.py`) runs one job per machine inside a **memory-capped scope** (60% of RAM,
  `OOMPolicy=continue`: one runaway prover is killed, not the job or the machine). It writes a heartbeat every
  30 s, enforces a wall-clock budget (the whole process tree is killed), kills leftover processes when a job
  ends, and resumes a job after a crash.
- **Timed-out work is not lost:** when an agent uses up its time box, what it wrote is still scored.

## How to run it
```
python3 fv/agent/fv_runtime.py submit <component> [--checkout <path>] [--machine node7] [--budget-hours 10]
python3 fv/agent/fv_runtime.py worker --once        # or leave a worker running on each machine
python3 fv/agent/fv_runtime.py status --machines this,node7
```
or one component directly: `python3 fv/agent/fv_agent.py verify <checkout> <component>`.
Each step can also be run on its own: `python3 fv/driver/fv_driver.py {extract|sync|classify|prove|check} ...`.

## What it produces
`components/<c>/verif/`: spec_properties.yaml, code_properties.yaml, unified_properties.yaml (every status, with
the scorer's evidence), discordances.yaml, and two pages: `<c>_scoring.html` and `<c>_discordances.html`. Every
step leaves a provenance record in `verif/.run/driver/`.

## What we measured (first unattended runs, 2026-10-09/10, two components it had never seen)
- zyre: Creusot 69 proved / 5 refuted / 5 open; Kani 11 / 5 / 63. block-device-kernel: Creusot 89 proved /
  1 refuted / 0 open; Kani 23 / 0 / 64.
- Real bugs found: zyre passes configuration text to the C library as a printf format (Kani); block-device-kernel
  does not cancel a timed-out request when the queue is full (Creusot).
- Cost: writing proofs takes an agent 1.5-2 h per tool on a new component; checking them takes seconds per
  Creusot proof and seconds to minutes per Kani harness.
- The first runs found 8 defects in this machinery (all fixed): see the notebook, fixes X150-X156.

## Known limits
- Kani coverage on a new component is limited by harness size and state cost (finding F23). We accept Creusot
  plus the Kani we have, and use Kani as an independent second check.
- One sync pass misses some disagreements; repeated passes keep finding new ones (finding F19).
