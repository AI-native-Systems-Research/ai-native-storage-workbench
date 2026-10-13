# FV - formal verification of system components with AI agents

Verifies a Rust component of Certus against its specification, finds real bugs, and repairs them with a proof that the
fix is right. AI agents (headless Claude Code) do the reading, the proof writing and the fixing; **code decides every
verdict** - an agent never grades its own work.

Tools: **Creusot** (deductive proofs, unbounded), **Kani** (bounded model checking on the real code), **Loom** (every
thread interleaving of a concurrent component). Results so far: 13 of 22 components verified; 6 real bugs in 11 places
found, fixed and re-proved.

## How the pieces fit
```
fv/agent/fv_runtime.py   RUNS JOBS unattended: a job queue per machine, a memory-capped scope per job, a wall-clock
     │                   budget, a heartbeat, resume after a crash, one status table across machines
     ▼
fv/agent/fv_agent.py     the FV AGENT: verifies ONE component end to end -
     │                   extract -> level1-check -> classify -> prove (each tool) -> check -> supervisor -> publish
     ▼
fv/driver/fv_driver.py   the STEPS. Each step starts one Claude session, limited by hooks (what it may write, what it
                         may run), then checks the result in code. Steps: extract, level1-check, classify, prove, check, repair.
                         The REPAIR AGENT is the `repair` step (not yet called by the FV agent; run it directly).

fv/skills/...            what the agents are told (methodology), and the GATE - the scripts that score and decide
```
- The **FV agent** is `fv_agent.py`; the **repair agent** is `fv_driver.py repair`.
- Full descriptions: [agent/FV_AGENT.md](agent/FV_AGENT.md) (the FV agent and runtime) and
  [driver/REPAIR.md](driver/REPAIR.md) (the repair agent).

## Before you start (needed by every step)
- **A Certus checkout on a LOCAL branch** (`git switch -c ...`): the driver refuses a detached published commit and never
  pushes.
- **Python 3.9+ with PyYAML**, and the **Claude Code CLI** (`claude`) on `PATH`.
- **Provers** (for prove and repair): Creusot with why3 and its SMT provers, and cargo-kani 0.67.0 (see the
  `tools-creusot-install` and `tools-kani-install` skills). Put them on PATH:
  `export PATH="$HOME/.local/share/creusot/bin:$HOME/.cargo/bin:$PATH"`. Loom needs nothing installed (a crate).
- **A user systemd manager** (`systemd-run --user --scope true` must work): the Kani scorer and the runtime run every
  prover inside a memory-capped scope and refuse to run without one.
- **Native libraries** for components that need them (SPDK, libzyre): set `CERTUS_DEPS_DIR` (default: the main
  checkout's `deps/`, then `/opt`); `ZYRE_BUILD_DIR` for zyre.

Each step leaves a record in `components/<c>/verif/.run/driver/<time>-<step>/` (prompt, agent log, hook log,
provenance).

## The steps

### 1. extract - what properties does the component promise?
Two **blind readers**: one sees only the spec, the other only the code; each lists the verifiable properties. A third
agent pairs the two lists; the driver builds the merged list from that pairing and checks every id is used once.
- **Needs:** `components/<c>/specs/`, `components/<c>/src/`, `components/interfaces/src/`.
- **Uses:** skill `skills/extract-verifiable-properties/SKILL.md` (copied into each reader's sandbox), skill
  `skills/build-property-inventory/SKILL.md`, prompts `driver/prompts/reader.md` and `reconcile.md`.
- **Writes:** `verif/spec_properties.yaml`, `verif/code_properties.yaml`, `verif/reconcile_pairing.yaml`,
  `verif/unified_properties.yaml` (the bundle every later step uses). Re-uses the reader lists from the same commit;
  `--fresh` forces new readers.
```
python3 fv/driver/fv_driver.py extract ~/certus-checkout zyre
```

### 2. level1-check - do spec and code really agree? (Level 1)
Right after extraction, an agent re-reads the code for every pair the reconciliation called "spec and code agree". A
pair that does not really agree becomes a disagreement (its old label is kept). It also lists the code's **hidden
assumptions** - things the code relies on that the spec does not say (e.g. "the message contains no newline"); proofs
are made under them, and they are reported. The driver validates the agent's report and applies it. **Nothing in the
spec or the code is changed** - this is not spec-kit's sync, which updates the spec. (The old name `sync` still works.)
- **Needs:** the bundle from extract.
- **Uses:** prompt `driver/prompts/level1_check.md`, skill `skills/build-property-inventory/SKILL.md` (section
  "LEVEL 1 - the spec<->code check"); gate scripts `level1.py`, `render_discordances.py`.
- **Writes:** reclassified records (with their old origin kept), `domain_discordances` (code assumptions),
  `verif/discordances.yaml`, `<c>_discordances.html`.
```
python3 fv/driver/fv_driver.py level1-check ~/certus-checkout zyre
```

### 3. classify - what to do with each disagreement?
Two independent classifier agents sort every disagreement: **A** prove it, **B** a bug for the repair agent, **C**
another tool. Where they disagree, it goes to a person.
- **Needs:** the bundle after level1-check.
- **Uses:** prompt `driver/prompts/classifier.md`, rubric `skills/component-verify/gate/classify_rubric.yaml`; gate
  `classify_merge.py`, `level1.py`, `render_discordances.py`, `check_done.py`.
- **Writes:** `classification` in the bundle, `level2_excluded` (what the provers do not attempt), refreshed pages.
```
python3 fv/driver/fv_driver.py classify ~/certus-checkout zyre
```

### 4. prove - prove the properties, one tool at a time
An agent writes proofs (Creusot) or harnesses (Kani), each with a **mutant twin that must fail**. The **scorer** then
re-runs everything itself and alone writes the status (proved / refuted / delegated / open). If the agent runs out of
time, what it wrote is still scored.
- **Needs:** the bundle after classify; the provers on PATH; for Kani, a working user systemd scope.
- **Uses:** prompt `driver/prompts/prove.md`; skill `skills/tools-verify-creusot-with-properties/SKILL.md` or
  `skills/tools-verify-kani-with-properties/SKILL.md`; gate `scorer_creusot.py` / `scorer_kani.py`, `cross_check.py`,
  `level1.py`, `render_discordances.py`, `render_scoring.py`.
- **Writes:** the proof crate `components/<c>/verif-creusot/` or `verif-kani/` (created for a new component), the
  statuses in the bundle, `<c>_scoring.html`.
```
python3 fv/driver/fv_driver.py prove ~/certus-checkout zyre --tool creusot --all
python3 fv/driver/fv_driver.py prove ~/certus-checkout zyre --tool kani --ids ZYR-PING-RETURNS-PONG
python3 fv/driver/fv_driver.py prove ~/certus-checkout zyre --tool kani --all --score-only   # re-score, no agent
```

### 5. check - is it done?
The definition of done, in code: every disagreement classified, every property scored by the scorers, no proof older
than a code assumption it relies on, no contradiction between the tools, the pages present.
- **Needs:** the bundle. **Uses:** gate `check_done.py`. **Writes:** nothing; exit code 0 = DONE, else it lists what
  is missing.
```
python3 fv/driver/fv_driver.py check ~/certus-checkout zyre
```

### 6. repair - fix a bug that verification found, and prove the fix (the repair agent)
One agent session per attempt (at most 2): explain the problem and give a verdict (code wrong / spec wrong / needs a
design decision), write a test that **fails on today's code**, fix the root cause, update the proof model and prove the
property, write a report. The **acceptance check** decides: builds, tests pass 5 times out of 5, the new test fails on the
old code, the property is proved (Creusot or Loom) with a failing twin, the model was updated, and no property proved
before is lost.
- **Needs:** the component's bundle AND its proof crate (Creusot `verif-creusot/` or the old `verif/` layout; Loom
  `verif-loom/` for `--loom`) **committed** on the local branch - the check compares against that commit. Native
  libraries and the creusot link are set up by the step itself.
- **Uses:** prompt `driver/prompts/repair.md`; acceptance scripts in `skills/component-repair/`:
  `repair_accept_repo.sh` (the whole repository), `repair_accept.sh` (a Creusot target), `repair_accept_loom.sh` (a
  Loom target), `layout.sh`; the gate's scorers for re-verification.
- **Writes:** on acceptance a **local commit** and `verif/fixes/fix_<component>_<finding>.md` (In short, the spec lines,
  the code before/after, the test, the check's evidence, effort). A "not a code bug" or "blocked" verdict gets a record
  in the run folder instead; nothing is committed.
```
python3 fv/driver/fv_driver.py repair ~/certus-checkout dispatch-map \
    --property DM-OLDEST-KEYS-NO-EVICTION-POLICY-EMPTY,DM-CREATE-MEMORY-TIER-ENTRY-NO-EVICTION-POLICY-ERROR \
    --finding get-pool-id-panic --callees get_pool_id,dm_oldest_keys,dm_create_memory_tier_entry
python3 fv/driver/fv_driver.py repair ~/certus-checkout extended-metadata-store \
    --property FLUSH-BG-TRIGGERS --finding flush-threshold-unimplemented --loom
# several repairs, one after another, each in a memory-capped scope:
fv/driver/fv_repair_queue.sh 200G "$HOME/certus-checkout|zyre|ZYR-CREATE-NODE-APPLIES-HEADERS|printf-format|apply_config,apply_headers"
```
Useful settings: `FV_GIT_NAME` / `FV_GIT_EMAIL` (author of the local commit), `REPAIR_TEST_RUNS` (default 5),
`REPAIR_TEST_FEATURES` (a component's test features; default: `testing` if the component defines it).

## The whole component, unattended
```
python3 fv/agent/fv_agent.py verify ~/certus-checkout zyre          # steps 1-5 + a bounded supervisor
python3 fv/agent/fv_agent.py publish ~/certus-checkout zyre         # two LOCAL verif branches; never pushes
python3 fv/agent/fv_runtime.py submit zyre --checkout ~/certus-checkout --budget-hours 8 [--machine node7]
python3 fv/agent/fv_runtime.py worker --once                        # run the queued job (memory-capped)
python3 fv/agent/fv_runtime.py status --machines this,node7
```

## Layout
```
fv/agent/            fv_agent.py (the FV agent), fv_runtime.py (unattended jobs), FV_AGENT.md
fv/driver/           fv_driver.py (the steps), hooks.py (guard + stop hooks), prompts/, fv_repair_queue.sh, REPAIR.md
fv/skills/component-verify/gate/      the gate: scorers, level1, classifier merge, cross_check, check_done, renderers
fv/skills/build-property-inventory/   the blind readers and the reconciliation (methodology)
fv/skills/extract-verifiable-properties/
fv/skills/tools-verify-{creusot,kani}-with-properties/   the proving methodology
fv/skills/component-repair/           the acceptance scripts; agent/ holds the earlier generated repair agent (superseded)
fv/skills/refute-tool-boundary/, tools-aggregate-coverage-by-interface/, component-sweep/
fv/skills/tools-{creusot,kani}-install/   prover setup
fv/install.sh        installs the skills into a target checkout (for the older skill-driven way of working)
```
Settings: `FV_CREUSOT_BIN` (Creusot's binaries; default `~/.local/share/creusot/bin`), `CERTUS_DEPS_DIR` (native
libraries), `FV_RESULTS_DIR` (dated result copies), `FV_JOBS` (the runtime's job folder; default `~/fv-jobs`).
Toolchain of the published results: cargo-kani 0.67.0, why3 1.8.2, Creusot 9cf662ce6.

## Status
Imported from `ai-native-storage-certus` (`.claude/skills/`); the copies there are frozen - develop here.
Changes are listed in [CHANGELOG.md](CHANGELOG.md), one line per push.
