You REPAIR one verification finding in the component `{component}`. Your work is judged by an acceptance check
that runs after you stop, outside your control; you never decide that the repair is accepted.

THE FINDING
- property: {property}
- statement: {statement}
- where it comes from: {traces}
- what the tools found: {tool_evidence}
- finding id: {finding}
{previous}
DO THESE STEPS, IN ORDER:
1. **Explain the problem** in {notes}/problem.md: one plain sentence a non-specialist can read, then the spec
   location and the code location (file:line) that disagree, and your verdict: code wrong / spec wrong / both /
   needs a design decision. If it is NOT "code wrong", stop after this file and say so - do not change code.
2. **Write a test that reproduces it** - it must FAIL on today's code by a panic or failed assertion (not by a
   compile error), using the existing API. Put it in its own file
   `components/{component}/src/<module path>/repair_test_<name>.rs`, wired into the module it tests by
   `#[cfg(test)] #[path = "repair_test_<name>.rs"] mod repair_test_<name>;`. Run it and see it fail.
3. **Fix the code** in `components/{component}/src/`. Fix the root cause wherever it occurs, not only the line
   the tool pointed at. Keep the change as small as the root cause allows. Run the test and see it pass; run the
   crate's `cargo test` FIVE times (the check does too: a flaky crash in any run rejects the repair).
{proof_step}
5. **Write {notes}/report.md** with these sections (plain English; it becomes the fix record and the PR text):
   `## In short` (AT MOST 30 words, two lines: `Problem: ...` and `Fix: ...`, for a slide) /
   `## Bug` (one sentence) / `## Impact` (what a user of the component would see) / `## How to reproduce`
   (2-3 steps a colleague can run) / `## Fix` (what changed, and the alternatives you rejected and why) /
   `## Scope beyond the finding` (any change not needed for THIS property - say "none" if none) /
   `## Risk` (behaviour change, compatibility) / `## Follow-ups`.

RULES
- Write ONLY: components/{component}/src/**, components/{component}/tests/**, components/{component}/verif-creusot/**,
  components/{component}/verif-kani/**, and {notes}/**. Never edit the property bundle, specs/, Cargo.lock outside
  the component, another component, or any acceptance script. Never commit, push, or run `cargo creusot clean`.
- Run commands in the foreground with a timeout; at most 2 prover processes at a time.
- If something outside your rules blocks you (a missing library, a disabled kernel feature, a missing toolchain
  link), stop and write it in report.md under `## Blocked` - do not work around it.
Finish with one line: REPAIRED {pid} | BLOCKED - <reason> | NOT A CODE BUG - <verdict>
