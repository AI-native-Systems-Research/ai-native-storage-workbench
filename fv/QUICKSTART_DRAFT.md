# Quick start for a new user (DRAFT 2026-10-22 - not yet linked from README.md)

This page tells you how to verify one Certus component with the tools in `fv/`. The full description of each step
is in [README.md](README.md). Read "Before you start" there first.

## 1. Install the tools
- **Creusot** (with why3 and the SMT provers): skill `skills/tools-creusot-install/`.
- **Kani** 0.67.0: skill `skills/tools-kani-install/`. Use this exact version, or your results cannot be compared with ours.
- **Loom**: nothing to install. It is a Rust crate (`loom = "0.7"`); each `verif-loom/` crate declares it.
- **Spin** 6.5.2: `modelling/spin/install-spin.sh` in the Certus repository (Daniel's script).
- **Claude Code** (`claude`) on `PATH`, and Python 3.9+ with PyYAML.
- Every long job must run in a memory-capped scope: `systemd-run --user --scope -p MemoryMax=60G ...`. One CBMC run
  (inside Kani) once used 117 GB and stopped a whole machine.

## 2. Verification branches: what to do when they already exist
Each earlier run left branches named `verif/<tool>/<component>-<date>` (in the Certus repository for older runs; in
this workbench repository from now on). `<tool>` is `creusot`, `kani`, `loom` or `spin`.

1. **Look first.**
   `git fetch origin && git branch -r | grep "verif/.*/<component>"`
   Each branch has a date in its name. Its README (or `SOURCE.md`) names the Certus commit that it checked.
2. **Do not delete an old branch, and do not reuse it.** It is the evidence of what was proved on that code. A new
   run does not replace it: it gets its own branch, so that the two can be compared.
3. **Start from the latest code.** Make a new local branch from the newest `unstable`:
   `git fetch origin && git switch -c verify/<component>-<date> origin/unstable`
   Do not continue on a branch that is weeks old. The specifications change often (spec-kit sync), and an old
   branch makes you verify old code without anyone deciding to.
4. **Start from a clean tree.** In the new work tree, remove the old proofs and harnesses (`verif-creusot/`,
   `verif-kani/`, `verif-loom/`, or the old `verif/` layout) so that every proof is written again for the new
   code. An old proof does not count for the new code.
5. **Make the new branches** with `fv_agent.py publish` (section 3). Today it makes `verif/creusot/<component>-<date>`
   and `verif/kani/<component>-<date>` as LOCAL branches in the Certus checkout, and it never pushes. Push them
   yourself. (Planned: also `verif/loom/...` and `verif/spin/...`, in this workbench repository.)
6. **Compare with the last branch.** For each property, give one result: proved again, lost (a regression), or
   newly proved. Write it in the README of the new branch.
7. **Update `fv/components/<component>/`** in this repository with the new models and the Certus commit.

## 3. Verify one component
From a Certus checkout on a LOCAL branch (step 2.3):
```
python3 fv/agent/fv_agent.py verify <certus-checkout> <component>          # default tools: creusot,kani
python3 fv/agent/fv_agent.py publish <certus-checkout> <component>         # makes the local verif branches
```
This runs extract, level1-check, classify, prove and check, unattended. Or run each step yourself with
`fv/driver/fv_driver.py <step> <checkout> <component>` (see README.md, "The steps").

The result is the bundle `components/<component>/verif/unified_properties.yaml` and the pages in the same folder.
Only the scorer writes a result. An agent never does.

## 4. Repair a bug that verification found
```
python3 fv/driver/fv_driver.py repair <checkout> <component> --property <ID> --finding <short-name> [--loom]
```
- The test comes first: it must fail on the current code, and its twin must also fail. For a Loom repair, commit
  the Loom test into the base before you start the repair.
- The acceptance check (`skills/component-repair/repair_accept_repo.sh`) decides. It builds, runs the tests 5 times,
  proves the target, and checks that no earlier proof is lost.
- Then a spec-kit sync (`/component-sync-specs`), approved by a person, and a pull request. A maintainer merges it.

## 5. Concurrency: Loom and Spin
- **Loom** tests live in `components/<c>/verif-loom/`. They run on a mirror of the concurrent code. The file
  `mirrored_sources.sha256` records the real files that the mirror copies; check it before every run. Each test
  has a twin (`__mutant`) that must fail. Skill: `skills/tools-loom-test/`.
- **Spin** models: skills `skills/tools-spin-model/` (write a new model) and `skills/tools-spin-sync/` (update a
  model to the current code). Each model has a `_control` copy with a minimal fix that must pass. Examples:
  `fv/components/dispatcher/spin/`.

## 6. Unattended jobs with an open model (GLM-5.3) - optional
Claude Code's auto mode does not work through a non-Anthropic gateway. Use `--permission-mode dontAsk` with an
allow list instead. (Scripts `glm-night` and `jrun`; to be added here.)

## Open items for this page
- A Spin step in the pipeline (today we run Spin jobs by hand).
- `fv_agent.py publish`: add the Loom and Spin branches, and write them to the workbench repository.
- The GLM scripts.
