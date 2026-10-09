You run the Level-1 SYNC CHECK for the component `{component}`. You may READ anything, but WRITE only one file:
`{out}`. Do not edit the bundle or any other file - the driver applies your report.

Read first and follow exactly the section "LEVEL 1 — the spec<->code sync check" of {skill}
(check 1 = merge check, check 2 = input-range check in BOTH directions).

Inputs: the bundle {bundle} (its properties with `origin: spec+code` are the merged records to check), the
specification under {specs}, the code under {src}, the interfaces under {interfaces}.

1. MERGE CHECK. For every `origin: spec+code` record, read its two sides (paired_from / derived_from and the blind
   files {spec_props} and {code_props}). If the merge narrowed, broadened or reworded either side's claim to make them
   agree, list it under `reclassify_divergent` with both readings in `divergence_note` (cite spec and code pointers).
2. INPUT-RANGE CHECK, both directions. List the constraints the SPEC places on inputs/configuration/state, and
   independently the assumptions the CODE makes (a mask assumes a power of two, a fixed buffer a maximum, a division
   non-zero, an `as u32` a range, an unchecked counter that it never overflows). Each mismatch becomes ONE
   `domain_discordances` entry - a CODE ASSUMPTION the spec does not state (not a fault). Its plain-English `assume`
   must say WHERE it holds (e.g. "at every call of format()", "before every clock tick"); `assume_rust` is ONE Rust
   boolean expression a proof can use as a precondition; give the methods it affects.
Only report what you can point at in the code. Report nothing rather than guess.

Write `{out}` as YAML:
```
component: {component}
reclassify_divergent:
  - id: <existing property id with origin spec+code>
    divergence_note: "SPEC (<pointers>): ... CODE (<file:line>): ..."
domain_discordances:
  - spec_says: <what the spec allows>
    code_does: <what the code assumes, with file:line>
    spec_pointers: [ ... ]
    code_pointers: [file:line, ...]
    methods: [ ... ]
    assume: <plain English, stating where it holds>
    assume_rust: <one Rust boolean expression>
```
Both lists may be empty. It must parse with yaml.safe_load. Finish with one line:
SYNC <component>: <r> reclassified, <d> code assumptions.
