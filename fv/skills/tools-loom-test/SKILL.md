---
name: tools-loom-test
description: Write Loom tests (every thread order) for concurrency properties of a Certus component, each with a twin that must fail, on a checksummed mirror of the concurrent code
argument-hint: "<component> <property ids from the bundle>"
---

# Write Loom tests for concurrency properties

Loom runs a small Rust program under **every** order of its threads: at each lock, atomic operation and channel
operation, it tries each thread that can run. It does not repeat two orders that give the same result (the method
of CDSChecker, Norris and Demsky, 2013). A pass means that no thread order breaks the assertions. Use it for a
property that depends on the timing of two to four threads. For one call at a time, use Creusot or Kani.

Read first: `loom` 0.7 documentation ("Writing tests", "Debugging Loom Failures", "Limitations and Caveats").

## Input
- The component `<c>` and the target property ids, from `components/<c>/verif/unified_properties.yaml`. Read each
  target's `statement`, `traces` and `classification`.
- The production code `components/<c>/src/` is READ ONLY. Never edit it.

## Steps
0. **Check the mirror first.** If `components/<c>/verif-loom/` exists, run
   `sha256sum -c verif-loom/mirrored_sources.sha256` in `components/<c>`. If it fails, the code changed: update each
   mirrored function that changed to today's code, keep the other tests passing, and rewrite the checksum file.
   If it passes, keep the mirror and add only what the targets need.
1. **The crate.** Use (or create) the separate crate `components/<c>/verif-loom/`: its own `Cargo.toml` with
   `loom = "0.7"`, and `[workspace]` in it, so that it is NOT a member of the main workspace.
2. **The mirror.** Copy only the concurrent logic that each target needs: the locks, the atomics, the channels and
   the order of the steps. Copy it as closely as possible from `src/`. Replace the GPU, NVMe and FFI calls with
   small deterministic stand-ins; a stand-in can be told to fail (for example `fail_write_key = Some(2)`).
   - Each mirrored function has a comment `// mirrors src/<file>:<line>`.
   - Record the sha256 of every source file that you mirrored in `verif-loom/mirrored_sources.sha256`
     (`sha256sum` output format).
3. **The tests.** In `verif-loom/tests/loom_props.rs`, for each target that you can express:
   - `verify_<snake_id>`: asserts the property inside `loom::model`.
   - `verify_<snake_id>__mutant`: the same assertion on a deliberately broken copy (a `Mutation` switch in the
     mirror). It MUST fail. It shows that the test can fail.
   - If the property is false on the mirrored code, keep the test as written. A failing `verify_` test is a finding:
     the code breaks the property. Never weaken a test so that it passes.
   - Prefer 2 or 3 threads. Each thread more multiplies the search.
4. **The bound.** If the full search does not finish in 900 s, use `loom::model::Builder` with
   `preemption_bound = Some(3)` for that test only, and say so. A bounded pass is a weaker result.
5. **Run each test in the foreground, one at a time:**
   `cd components/<c>/verif-loom && RUSTFLAGS="--cfg loom" timeout 900 cargo test --release --test loom_props <name> -- --exact`
   Keep `RUSTFLAGS="--cfg loom"` local to this command. If it leaks into other builds, they fail.
6. **The advisory file.** Write `components/<c>/verif/loom_advisory.yaml`: for each target id,
   `{test, mirrors: [file:line], bound: none|<n>, your_result: pass|fail|not-expressed, why: one sentence}`.
   For a target that is NOT about thread order (for example a wall-clock deadline), write `not-expressed`; do not
   force a model.

Your results are advisory. A scorer runs the tests again and writes the results. The last line of your answer:
`LOOM: <n> tests written; pass <p>, fail <f>, not-expressed <x>`.

## For a repair
A Loom repair starts with this test, committed into the base before the repair:
- the `verify_` test must FAIL on the current code, and its twin must also fail;
- the repair agent cannot change the test;
- the acceptance check runs the test (it must pass) and the twin (it must still fail) on the repaired code.
Example: `fv/components/dispatcher/loom/tests/loom_props.rs` (`flush_reports_failed_write`).

## What we learned (2026-10-12 .. 22)
- A Loom test needs a twin that must fail. Without it, a passing test can check nothing.
- The mirror drifts when the code changes. The checksum file stops a test that runs on an old mirror.
- Loom does not implement the full C11 model: it treats `SeqCst` as `AcqRel`. A busy loop without
  `loom::thread::yield_now()` can run without end.
- Loom and Spin find different bugs. Spin checks a design model with many actors; Loom checks the Rust code with
  few threads. See `fv/components/dispatcher/README.md`, insights 1 and 5.
