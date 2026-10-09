# FV - formal verification of system components with AI agents

A pipeline that verifies a Rust component against its specification with two provers, Creusot (deductive, unbounded)
and Kani (bounded model checking on the real code), driven by Claude agents and judged by deterministic scripts.

## The method in five steps
1. **Two blind readers.** One agent reads only the specification, another only the code; each writes the properties it
   finds (`spec_properties.yaml`, `code_properties.yaml`). Reconciling them gives the agreed properties and the
   candidate disagreements (`unified_properties.yaml`).
2. **Level 1 - read before proving.** The sync check finds **discordances** (spec and code say different things) and
   **code assumptions** (assumptions the code makes that the spec does not state - not faults; properties are proved
   under them). Two independent classifier agents sort each candidate: **A** the code does it (prove it), **B** real
   disagreement (repair agent), **C** needs another tool (Loom, Spin, a test). A disagreement between them counts as B.
3. **Level 2 - prove what both sides agree on**, with Creusot and Kani. Agents write the proofs; they never write a
   status.
4. **The gate decides.** The scorers re-run every proof from source and alone write the verdict. Every proof needs a
   mutant twin that must fail; a property proved by one tool and refuted by the other fails the run; a refutation must
   start from a reachable state; a proof may assume only the requirement's words, a declared code assumption or a
   proved invariant. `check_done.py` says DONE or lists what is missing.
5. **Repair (B rows).** The repair agent writes a failing test first, fixes the code (never the spec), and re-verifies
   that nothing already proved is lost.

## Layout
```
fv/skills/component-verify/          the orchestrator skill + gate/ (scorers, level1, classifier merge, cross_check,
                                     check_done, renderers)
fv/skills/build-property-inventory/  the two blind readers and the reconciliation
fv/skills/extract-verifiable-properties/
fv/skills/tools-verify-{creusot,kani}-with-properties/   the proving agents
fv/skills/refute-tool-boundary/      an independent check of every claimed tool limit
fv/skills/tools-aggregate-coverage-by-interface/          the per-component pages
fv/skills/component-sweep/           a cheap breadth-first pass over many components
fv/skills/component-repair/          the repair agent (agent/ holds a frozen copy of the agent kit it was built with)
fv/skills/tools-{creusot,kani}-install/                   prover setup
fv/install.sh                        installs the skills into a target checkout
```
Planned: `fv/driver/`, a Python orchestrator on the Claude Agent SDK that runs the steps in order until `check_done`
says DONE.

## Use
```
fv/install.sh /path/to/ai-native-storage-certus      # copies the skills into its .claude/skills/
# then, in a Claude Code session in that checkout:  /component-verify <component>
```
Settings: `FV_CREUSOT_BIN` (where Creusot's binaries live; default `~/.local/share/creusot/bin`), `CERTUS_DEPS_DIR`
(native libraries for the repair agent's builds; default `/opt`), `FV_RESULTS_DIR` (where to keep dated result copies).
Toolchain the published results were produced with: cargo-kani 0.67.0, why3 1.8.2, Creusot 9cf662ce6.

## Status
Imported from `ai-native-storage-certus` (`.claude/skills/`, source commit noted in the import commit). The copies in
that repository are frozen; develop here.
