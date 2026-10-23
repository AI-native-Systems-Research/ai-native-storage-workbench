# dispatcher — verification models and results

The dispatcher of [Certus](https://github.com/AI-native-Systems-Research/ai-native-storage-certus) moves cache blocks
between GPU memory, a DRAM memory tier and NVMe SSDs, with background threads that write to SSD and free memory.
It has 22 interface methods and about 10,000 lines of Rust. We verified it with four tools. This folder holds the
models that each tool checks, and what we learned from them.

**State:** Certus `unstable` after PR #562 (merged 2026-10-22), plus the accepted repair of root cause #11 (not merged
yet; see "Repairs"). Each tool folder names the Certus commit that its model follows.

## The four models at a glance

| folder | tool | what the model is | what it checks | main risk |
|---|---|---|---|---|
| `creusot/` | Creusot (deductive) | a logic model of each function, with contracts in Pearlite (`#[requires]`, `#[ensures]`); the cache state is a logical map | a property for **all** inputs, one call at a time | the model differs from the code |
| `kani/` | Kani (bounded) | harnesses on a small copy of the code (`mirror.rs`, `model.rs`) | **all** inputs up to a bound | the bound is too small, or a path is pruned (`kani::cover!` guards it) |
| `loom/` | Loom (all thread orders) | a mirror of the concurrent logic, with stand-ins for the GPU and the SSD; `mirrored_sources.sha256` records the real files it copies | **every** order of 2–4 threads (or a bounded search, stated in the test) | the mirror differs from the code (the checksum stops the test) |
| `spin/` | Spin (design model) | a hand-written Promela model of the protocol, 3–6 actors | **every** state of the model, safety and liveness | the model leaves out a value or an actor |

`unified_properties.yaml` is the property bundle: 224 properties (198 verifiable), each with its source in the
specification or the code, and the result of each tool.

Every proof has a **twin** (a copy made false on purpose) that must fail. Every repair starts with a test that fails
on the old code. An independent scorer, not the agent, writes every result.

## Results

| tool | result |
|---|---|
| Creusot | 110 proved, 17 counterexamples, 43 open (of 170), 2026-10-15; 111 proved after the repair of #11 |
| Kani | 20 of 20 bug candidates confirmed (4 s to 6 min each) |
| Loom | 11 concurrency properties: 6 hold, 5 counterexamples (2026-10-15); one test for each repair since then |
| Spin | Daniel's 8 models passed, but 4 described old code; re-synced, all 4 failed (J20). 5 new models (J22–J26) |

The counterexamples group into **32 root causes**: 10 confirmed bugs (all repaired and merged), 1 specification
defect, 21 not checked yet.

## Insights

1. **No single tool is enough.** Each tool found bugs that the others did not find. Spin found 2 bugs that need three
   actions at the same time (a lookup, an eviction and a removal); our Loom tests had two. Loom found a wrong "served
   from" label that Daniel's Spin model of the same race could not see, because the model had no label.
2. **A model is only as good as its sync with the code.** 4 of Daniel's 8 Spin models passed for months but described a
   protocol that the code no longer had. Updated to the current code, each failed in less than 0.05 s.
3. **A model must contain every value that the code returns,** or list it under "Left out". With a 12-line label
   added, Spin found the label bug in 0.03 s (J18).
4. **A wrong model can prove a false property.** On another component (zyre), Creusot proved a property with a wrong
   model of a C function; Kani, with an independent model, refuted it. Our cross-tool check stops such a run.
5. **Spin first, Loom second.** Spin explores the protocol fast with many actors and gives candidates. A Loom test on
   the Rust code then confirms each candidate, and the same test proves the repair.
6. **An open model can write the models.** J24–J26 and the repair of #11 were done by GLM-5.3, not Claude. We ran
   every model again ourselves, with the same results.

## Spin models

One folder for each question; the full list, with the bugs each model shows, is in [spin/README.md](spin/README.md).

| model | what it checks |
|---|---|
| `spin/clear-memory-tier-pinned/` | `clear_memory_tier` with pinned lookups and the writer |
| `spin/flush-then-clear/` | `flush_to_ssd`, then `clear_memory_tier`, after a failed write |
| `spin/failed-promotion-slot/` | a promotion from SSD that fails |
| `spin/remove-ssd-extent/` | `remove` and the SSD space |
| `spin/promotion-lookup-race-label/` | two lookups race to promote one SSD key: the "served from" label |
| `spin/no-lost-extents/`, `spin/populate-lookup-linearizability/`, `spin/promotion-atomicity/`, `spin/write-before-evict/` | Daniel's 4 models, re-synced to the code |

Each folder has the model, its control copy with the minimal fix (it must pass), a `Makefile`, a README (properties,
correspondence with the code, returned values, left out) and the trails. Run a model with `make safety` (Spin 6.5.2;
the Makefile compiles `pan` with `-DMEMLIM=16000` and uses a memory cap). The model test of GLM-5.3 against Claude
(the same tasks, done twice) is recorded in the notebook, not here.

## Repairs

| root cause | bug | found by | repair proved by | state |
|---|---|---|---|---|
| #6 | `remove` fails during a write to SSD | Kani, Loom | Loom | merged #562 |
| #19 | the "served from" label is wrong after a concurrent promotion | Kani, Loom | Loom | merged #555 |
| #20 | eviction events are lost and not counted | Kani, Loom | Loom | merged #555 |
| #10 | `populate` does not release its memory after a failure | Creusot | Creusot | merged #555 |
| #2 | `copy_gpu_to_memory_completed` frees memory that the cache uses | Creusot | Creusot | merged #555 |
| #30 | a cold lookup releases its pin before the SSD read | Spin | Loom | merged #555 |
| #31 | the SSD evictor frees memory before the map removal | Spin | Loom | merged #555 |
| #1 | `clear_memory_tier` frees memory that a reader holds | Creusot, Spin | Loom | merged #562 |
| #5, #16 | `flush_to_ssd` returns `Ok` after a failed write; wrong count | Kani, Spin | Loom | merged #562 |
| A-07 | the counter `store_already_resident` counts a lost load race | spec review | Loom | merged #562 |
| #11 | a failed promotion does not release its memory slot | Creusot, Spin | Loom | merged #570 |
| #9, #12 | `remove` does not release the SSD space; releases a foreign pin | Spin | (Loom) | repair running |

## Known limits

- Kani and Loom check mirrors of the code; each repair test confirms its bug on the real code.
- Creusot and Spin check models; a result is a candidate until a test confirms it.
- The component `dispatcher-p2p` implements the same interface and has 3 of the same bugs (found by reading its code);
  not repaired yet.
- All results are from one component, by our team. Nobody outside the team has reproduced them.
