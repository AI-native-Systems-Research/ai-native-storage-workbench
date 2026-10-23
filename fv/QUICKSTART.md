# Quick start for a new user

This page tells you how to verify one Certus component with the tools in `fv/`. The full description of each step
is in [README.md](README.md). Read "Before you start" there first.

> **Note:** the pipeline (`fv_agent.py verify`, section 3) runs **Creusot and Kani** automatically.
> **Loom and Spin are not in the pipeline yet:** you run them by hand. See
> [section 5](#5-concurrency-loom-and-spin) for the commands.

## Example: verify one component with all 4 tools

Today this is 5 steps. Creusot and Kani run with one command; Loom and Spin need a Claude Code session and a person.
We are working to make it one command for all 4 tools (see "Open items" at the end). The example component is
`extended-metadata-store`; it has threads, so Loom and Spin are useful for it. Install the provers first (section 1).

```
# Once: clone this workbench and the Certus repository, next to each other (section 1)
git clone git@github.com:AI-native-Systems-Research/ai-native-storage-workbench.git ~/ai-native-storage-workbench
git clone git@github.com:AI-native-Systems-Research/ai-native-storage-certus.git ~/ai-native-storage-certus
#   later, before each verification: git -C ~/ai-native-storage-workbench pull

# 0. A new branch from the latest code (section 2)
cd ~/ai-native-storage-certus                         # your Certus clone: the folder that contains components/
git fetch origin && git switch -c verify/extended-metadata-store-20261024 origin/unstable
W=~/ai-native-storage-workbench/fv                     # this folder
bash $W/install.sh .                                   # copies our skills (tools-loom-test, tools-spin-model, ...) into
                                                       # .claude/skills/ of this clone, so the /commands below exist.
                                                       # It replaces skills of the same name there: do not commit
                                                       # .claude/ (git checkout -- .claude to undo it)

# 1. Creusot and Kani: one command, unattended, several hours (section 3)
python3 $W/agent/fv_agent.py verify . extended-metadata-store

# 2. Loom: by hand (section 5, "Loom: write new tests")
claude
#   type: /tools-loom-test extended-metadata-store <property ids of class C in verif/discordances.yaml>
#   then run each test (section 5, "Loom: run the existing tests")

# 3. Spin: by hand (section 5, "Spin: write a new model")
claude
#   type: /tools-spin-model <the protocol or the suspected bug>
#   then, in the model's folder: make safety && make liveness

# 4. The branches: publish makes the Creusot and Kani branches, local only (section 3a)
python3 $W/agent/fv_agent.py publish . extended-metadata-store

# 5. Check the results, then push (section 3a)
#   open components/extended-metadata-store/verif/extended-metadata-store_scoring.html
git push origin verif/creusot/extended-metadata-store-20261024 verif/kani/extended-metadata-store-20261024
```

**Limits today:**
- No command makes the Loom and Spin branches yet. Commit their files on a branch by hand.
- The Loom and Spin results are not in the property list or the HTML page yet.
- To repair a bug that a tool found, see section 4.

## 1. Install the tools
- **This workbench** (our pipeline, scripts and skills), next to your Certus clone:
  `git clone git@github.com:AI-native-Systems-Research/ai-native-storage-workbench.git ~/ai-native-storage-workbench`
  Run `git pull` in it before each verification, so that you use the newest version.
- **Our skills, in your Certus clone:** `bash ~/ai-native-storage-workbench/fv/install.sh <certus-checkout>`.
  It copies the skills in `fv/skills/` (verify, repair, Loom, Spin, install) into `<certus-checkout>/.claude/skills/`,
  so that Claude Code finds them there (for example `/tools-loom-test`). It replaces skills of the same name; do
  not commit `.claude/` (`git checkout -- .claude` undoes it). The pipeline commands (`fv_agent.py`) run from the
  workbench and need no install.
- **Creusot** (with why3 and the SMT provers): skill `fv/skills/tools-creusot-install/`.
- **Kani** 0.67.0: skill `fv/skills/tools-kani-install/`. Use this exact version, or your results cannot be compared with ours.
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
   The newest branch of each component is listed in [components/README.md](components/README.md).
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

**What `verify` does, in this order** (unattended; a large component takes several hours):
1. **extract:** two Claude agents read the specification and the code separately, and write the property list
   (`components/<c>/verif/unified_properties.yaml`). It is skipped if the list already exists.
2. **level1-check:** finds where the specification and the code disagree.
3. **classify:** two agents put each disagreement in a class: A (prove it with Creusot), B (a bug candidate: Kani,
   then a repair), C (another tool: Loom or Spin), or ? (a person decides).
4. **prove:** Creusot, then Kani, on the properties. A scorer runs every proof again, with a broken twin that must
   fail, and only the scorer writes a result.
5. **check:** if some properties are still open, a supervisor agent chooses: retry some of them with a tool, or stop.
   It does this for at most 2 rounds (`--rounds`).
6. **The end:** `DONE`, or `stopped` with the reason. The pages (`components/<c>/verif/*.html`) show each property
   with its result: proved, counterexample, or open.

If you run `verify` again on the same commit, it reuses the steps that already finished (`--redo` repeats
level1-check and classify).

**What `verify` does NOT do:**
- It does not create branches. Run `publish` for that: it makes `verif/creusot/<c>-<date>` and
  `verif/kani/<c>-<date>` as LOCAL branches, and never pushes. Push them yourself.
- It does not repair a bug. A counterexample stays in the bundle. To repair it, see section 4.
- It does not run Loom or Spin. See section 5.
- It does not change the product code. It writes only into `components/<c>/verif*/`.

Or run each step yourself with `fv/driver/fv_driver.py <step> <checkout> <component>` (see README.md, "The steps").

### 3a. The results, the branches and the push
**The results** (written by `verify`, in `components/<c>/verif/` of the checkout):
- `unified_properties.yaml`: every property, with its result from each tool and the scorer's evidence.
- `spec_properties.yaml` and `code_properties.yaml`: the two independent lists. One agent read only the
  specification, the other only the code. They are always kept (also on the branches), so that the extraction,
  Level 1 and the classifier can be tested again on them. This is for the test period of the pipeline (decision
  of 2026-10-23); later, an option may write them only when they are needed.
- `discordances.yaml`: where the two lists disagree, with the class of each disagreement (A, B, C or ?).
- `<c>_scoring.html`: one page with the result of each property (proved, counterexample or open), for each tool.
- `<c>_discordances.html`: where the specification and the code disagree.

**The branches** (made by `publish`, from the base commit of the checkout; LOCAL only):
- `verif/creusot/<c>-<date>`: the `verif/` folder (the YAML and the HTML) and the Creusot proof crate.
- `verif/kani/<c>-<date>`: the same `verif/` folder and the Kani proof crate.

**The push** (you do it; `publish` never pushes, so that a person looks at the results first):
1. Open `components/<c>/verif/<c>_scoring.html` in a browser, and check the results.
2. Push the two branches. Use the names that `publish` printed:
   `git push origin verif/creusot/<c>-<date> verif/kani/<c>-<date>`
3. Update the table of the newest branches: `python3 fv/tools/verif_branch_table.py <certus-checkout>`, and put its
   output in `fv/components/README.md`.

**Example**, with `logger`. `<certus-checkout>` is the folder of your Certus clone (the folder that contains
`components/`):
```
cd ~/ai-native-storage-certus
git fetch origin && git switch -c verify/logger-20261024 origin/unstable
python3 ~/ai-native-storage-workbench/fv/agent/fv_agent.py verify . logger
python3 ~/ai-native-storage-workbench/fv/agent/fv_agent.py publish . logger
#   open components/logger/verif/logger_scoring.html and check it
git push origin verif/creusot/logger-20261024 verif/kani/logger-20261024
```

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
Today you run Loom and Spin by hand. The pipeline (`fv_agent.py verify`) runs Creusot and Kani only, and the
property bundle does not record Spin results yet.

### Loom: run the existing tests
The Loom tests of a component are in the Certus repository, in `components/<c>/verif-loom/`. They run on a mirror
of the concurrent code.
```
cd components/<c>
sha256sum -c verif-loom/mirrored_sources.sha256        # 1. the mirror must match the current code
cd verif-loom
RUSTFLAGS="--cfg loom" cargo test --release --test loom_props -- --list            # 2. list the tests
RUSTFLAGS="--cfg loom" timeout 900 cargo test --release --test loom_props <test> -- --exact   # 3. run one test
```
- If step 1 fails, the code changed: update the mirror first (skill `skills/tools-loom-test/`, step 0). A test on
  an old mirror proves nothing about the current code.
- Run one test at a time. Set `RUSTFLAGS="--cfg loom"` only on the Loom command; if it leaks into other builds,
  they fail.
- Each `verify_<id>` test has a twin `verify_<id>__mutant`, and the twin must FAIL. A pass of the twin means that
  the test checks nothing.
- A test that says `preemption_bound = Some(3)` searches only the thread orders with at most 3 preemptions. That is
  a weaker result than a full search.
- The first build downloads the `loom` crate, so it needs internet access.

### Loom: write new tests
Use the skill `skills/tools-loom-test/` in a Claude Code session in the Certus checkout:
`/tools-loom-test <component> <property ids>`. For a repair, commit the new test into the base first: it must fail
on the current code.

### Spin: run an existing model
Spin 6.5.2 and a C compiler are needed (install: `modelling/spin/install-spin.sh` in the Certus repository).
Example, one of our models:
```
cd fv/components/dispatcher/spin/clear-memory-tier-pinned
make safety       # every state: prints "errors: 0", or writes <model>.pml.trail
make liveness     # the protocol does not get stuck (an LTL property, with weak fairness)
spin -t -p clear_memory_tier.pml     # replay a trail step by step, if there is one
```
- Each folder has a `_control` copy with the minimal fix. It must pass: this shows that a failure comes from the
  bug, not from the model.
- The `Makefile` runs `pan` in a memory-capped scope and compiles it with `-DMEMLIM=16000`. Keep these limits for a
  large model.
- Each model names the Certus commit that it follows. If the code has changed since, update the model first
  (`/tools-spin-sync`); an old model can pass and still miss a bug.

### Spin: write a new model
In a Claude Code session in the Certus checkout:
- `/tools-spin-model <bug or property>` writes a new Promela model from the code. Each step names its line in the
  code. The README of the model must list every value that the code returns, or put it under "Left out".
- `/tools-spin-sync` updates an existing model to the current code.

## 6. Unattended jobs with an open model (GLM-5.3) - optional
Claude Code's auto mode does not work through a non-Anthropic gateway. Use `--permission-mode dontAsk` with an
allow list instead. (Scripts `glm-night` and `jrun`; to be added here.)

## Open items for this page
- One command for all 4 tools: a Loom step and a Spin step in `fv_agent.py verify --tools creusot,kani,loom,spin`,
  their results in the property list and the HTML page, and `publish` making all 4 branches. Then the example at
  the top becomes 3 commands.
- After the test period of the pipeline: an option for `verify` / `publish` to keep the two independent property
  lists (`spec_properties.yaml`, `code_properties.yaml`) only when needed. Today they are always kept.
- A Spin step in the pipeline, and Spin results in the property bundle.
- `fv_agent.py publish`: add the Loom and Spin branches, and write them to the workbench repository.
- The GLM scripts.
