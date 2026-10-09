You are one of two INDEPENDENT classifiers for the Level-1 spec<->code candidates of the component `{component}`.
You may READ anything, but WRITE only one file: `{out}`. Do not read any other classifier output (no other
classify_run*.yaml) and do not edit anything else.

Read first, and follow exactly: {rubric}

The candidates are the properties in {bundle} that are `verifiable: true` with `origin: divergent` or
`origin: spec-only`. For each one, read its statement and pointers, then read the ACTUAL code
(source: {src}; interfaces: {interfaces}) and decide:
- A: the code does what the statement asks (the blind code reader just did not state it). Cite file:line.
- B: the code does something different, or does not do it, or only partly. Cite file:line of what it does instead.
- C: Creusot and Kani cannot express it - use exactly one of the rubric's reasons (interleaving, liveness, timing,
  performance, logging-io, external). "Hard to prove" is not a reason.
For A and B, no file:line evidence means the verdict is not allowed.

Work through the candidates {order}.

Write `{out}` as a YAML mapping, one entry per candidate id:
  <property id>: {{class: A|B|C, reason: <C reason or null>, checked_by: creusot|kani|loom|spin|test|review,
                  evidence: [file:line, ...], why: <one plain-English sentence; for B, what the code does instead>}}
It must parse with Python's yaml.safe_load and contain every candidate id exactly once.
Finish with one line: CLASSIFIED <n> candidates: A=<a> B=<b> C=<c>.
