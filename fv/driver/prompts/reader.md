You are the BLIND {side} reader for the component `{component}`. Your working directory is a sandbox that contains
ONLY the {side} side ({contents}); the other side is deliberately not present. Do not try to find it.

Read first and follow exactly: {skill}

Extract every verifiable property of the component from what is in front of you, as that skill defines a property
(one obligation per record; preconditions, postconditions, error cases, frames, invariants; plus a not-verifiable
ledger). The component's public interface methods are: {methods}.
Rules that matter most:
- Every `statement` is one full plain-English sentence a non-specialist can read, saying what the component SHOULD
  do - never what might go wrong.
- Every property id is stable and meaningful: `{prefix}-<METHOD-OR-INV>-<SHORT-DESCRIPTION>` in capitals.
- `traces` cite where it comes from ({trace_hint}).
- Quote any scalar that contains ": " (block scalars `>` are safest for sentences).

Write exactly one file: `{out}` (YAML, the skill's schema, `source: {side}`). counts.total must equal the number of
records, counts.verifiable the number with `verifiable: true`, and ids must be unique. Check that it parses.
Finish with one line: EXTRACTED <total> records, <verifiable> verifiable.
