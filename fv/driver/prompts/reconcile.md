You RECONCILE two blind extractions of the component `{component}`. You may read anything in the checkout.
WRITE only `{out}` - a small PAIRING TABLE of ids. The driver builds the unified property list from it mechanically
(statements, traces, methods are copied from the inputs), so do NOT rewrite any record.

Inputs: the spec reader's list {spec_props} and the code reader's list {code_props}, written independently.
Read first and follow the reconciliation sections of: {skill} ("Inputs - two BLIND extractions, then reconcile",
"Coverage by construction", "Reconciling at scale"). Read the component's code where it decides a pairing.

Put every input id (verifiable or not) in exactly one group. For each group of verifiable ids decide one origin:
- `spec+code` - the spec record(s) and code record(s) say the same thing (same condition, same outcome).
- `divergent` - they describe the same behaviour differently; give `note`: one or two plain sentences, what the
  spec says and what the code does, with the file:line that decides it.
- `spec-only` - the spec requires it and no code record states it (code: []).
- `code-only` - the code guarantees it and the spec says nothing about it (spec: []).
Not-verifiable ids go in groups with `origin: not-verifiable` (one id per group is fine).
Do NOT pair two records by narrowing, broadening or rewording either side - that is `divergent`.
Optional per group: `id` (the unified id; default = the first spec id, else the first code id) and `statement`
(only when a merged group needs one sentence covering both sides; plain English, what the component SHOULD do).

Format (YAML; quote any scalar containing ": "):
groups:
  - {{origin: spec+code, spec: [<id>], code: [<id>, <id>]}}
  - {{origin: divergent, spec: [<id>], code: [<id>], note: "<what differs, with file:line>"}}
  - {{origin: spec-only, spec: [<id>], code: []}}
  - {{origin: code-only, spec: [], code: [<id>]}}
  - {{origin: not-verifiable, spec: [<id>], code: []}}

Write the file in ONE Write call and keep it short (ids, origins, notes only). The driver checks it mechanically: every
input id exactly once, no unknown id, every divergent group has a note. Finish with one line:
PAIRED <groups> groups: spec+code <a>, divergent <b>, spec-only <c>, code-only <d>, not-verifiable <e>.
