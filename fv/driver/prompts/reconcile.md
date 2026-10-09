You RECONCILE two blind extractions of the component `{component}` into one property list. You may read anything
in the checkout. WRITE only `{out}`.

Inputs: the spec reader's list {spec_props} and the code reader's list {code_props}, written independently.
Read first and follow the reconciliation sections of: {skill} (the property record schema, "Inputs - two BLIND
extractions, then reconcile", "Coverage by construction", "Reconciling at scale").

For every verifiable record of BOTH inputs decide exactly one of:
- `origin: spec+code` - the two say the same thing (same condition, same outcome). Record both sides.
- `origin: divergent` - they describe the same behaviour differently; keep both readings in `divergence_note`.
- `origin: spec-only` - the spec requires it and no code record states it.
- `origin: code-only` - the code guarantees it (a documented contract) and the spec says nothing about it.
Do NOT merge two records by narrowing, broadening or rewording either side - that is `divergent`.

Each unified record: id, methods (list, from the interface methods {methods}), kind, verifiable: true, statement
(plain English), origin, traces, and `paired_from: {{spec: [<spec ids>], code: [<code ids>]}}` naming every input id
it came from. Copy the not-verifiable ledger entries with `verifiable: false` and their `paired_from`.
Top level: component, pin: {pin}, interface_methods: {methods}, counts, properties.
INTEGRITY (the driver checks this mechanically): every input id appears in exactly one record's paired_from; no
paired_from names an id that does not exist. Check it yourself before finishing.
Write `{out}`; it must parse. Finish with one line: RECONCILED <n> records: spec+code <a>, divergent <b>, spec-only <c>, code-only <d>.
