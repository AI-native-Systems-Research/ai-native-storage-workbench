# dispatcher — Spin models

One folder for each question. Each folder has the model (`<name>.pml`), a control copy with the minimal fix
(`<name>_control.pml`, or `-DCONTROL` in the same file), a `Makefile`, a `README.md` and the trails of the
counterexamples. Older versions are in the git history; full runs are on the `verif/spin/dispatcher-<date>` branches.

All models follow Certus `acd24025`. Run one with `make safety` (and `make liveness` where the Makefile has it). On
the code as it was, each model finds its bug (`errors: 1`); its control copy passes (`errors: 0`). Checked on
2026-10-23.

| model | what it checks | bugs it shows (root cause, state) |
|---|---|---|
| [clear-memory-tier-pinned](clear-memory-tier-pinned/) | `clear_memory_tier` with two pinned lookups and the writer | #1 frees the memory of a pinned entry — PR #562 |
| [flush-then-clear](flush-then-clear/) | `flush_to_ssd`, then `clear_memory_tier`, after a failed write | #5, #16 data loss; wrong count — PR #562 |
| [failed-promotion-slot](failed-promotion-slot/) | a promotion from SSD that fails | #11 the memory slot leaks — PR #570 (open) |
| [remove-ssd-extent](remove-ssd-extent/) | `remove` and the SSD space | #9 the extent leaks, #12 a foreign pin is released — repaired, not merged; P2 (an extent freed twice) open |
| [promotion-lookup-race-label](promotion-lookup-race-label/) | two lookups race to promote one SSD key: the "served from" label | #19 DRAM labelled as SSD — PR #555 |
| [no-lost-extents](no-lost-extents/) | SSD space is never lost or freed twice | #9 — repaired, not merged; #32 open |
| [populate-lookup-linearizability](populate-lookup-linearizability/) | a store and a lookup of the same key | #30 a cold lookup drops its pin, #10 populate leaks its slot — PR #555 |
| [promotion-atomicity](promotion-atomicity/) | a promotion with concurrent lookups and the SSD evictor | #31 the evictor frees too early — PR #555; #11 — PR #570 (open) |
| [write-before-evict](write-before-evict/) | an entry is on SSD before it leaves the memory tier | #31 — PR #555; #22 open |

**Where the models come from:**
- `no-lost-extents`, `populate-lookup-linearizability`, `promotion-atomicity`, `write-before-evict`: Daniel's models
  (Certus `modelling/spin/`). They passed, but described old code; re-synced to the code, all 4 fail.
  `before_resync/` in each folder has Daniel's version and the diff.
- `promotion-lookup-race-label`: Daniel's model with the "served from" label added. Without the label, it passes.
- The other four: written from scratch, from the code. Two by Claude, two by GLM-5.3 (an open model).

The skills that write and update these models: `fv/skills/tools-spin-model/` and `fv/skills/tools-spin-sync/`.
