# promotion-lookup-race-label — two lookups race to promote one SSD key: is the "served from" label right?

- **What it checks:** when two `batch_lookup` calls promote the same SSD key at the same time, the loser waits for the
  winner and is served from DRAM. Its "served from" label must then say DRAM.
- **Result:** the model finds the bug (assertion L1 fails, 8,158 states, 0.01 s). The control copy, with the label
  changed to DRAM in the recovery pass, passes (58,970 states).
- **The bug:** root cause #19, "a key served from DRAM after a wait is labelled SSD". Loom found it first; this model
  shows that a Spin model finds it too, once it contains the label. Repaired in PR #555.
- **Certus commit:** `acd24025`.
- **Origin:** Daniel's model `modelling/spin/promotion-lookup-race` (Certus repository), plus the label: 9 lines added
  and 1 changed (`label.diff`). Without the label, Daniel's model passes, because it cannot see the bug.
- **History:** J18 (Claude, 2026-10-17) added the label. J24 (GLM-5.3, 2026-10-21) did the same task without seeing
  J18's answer, and found the same bug; that model test is recorded in the notebook, not here.

## Files
| file | what it is |
|---|---|
| `promotion_lookup_race.pml` | Daniel's model plus the label (the code as it was) |
| `promotion_lookup_race_control.pml` | the same, with the fix: the recovery pass sets the label to DRAM |
| `label.diff` | the change from Daniel's model to `promotion_lookup_race.pml` |
| `Makefile` | `make safety` (compiled with `-DMEMLIM=16000`, run in a memory-capped scope) |
| `trails/` | the counterexample trail, and its replay with `spin -t -p` |
| `REPORT.md` | the full J18 report: all 8 of Daniel's models, and Spin compared with Loom |

## Properties
| ID | Property | Type |
|---|---|---|
| A1 | At most one lookup wins `mt.insert` per promotion: the slot is not allocated twice. | Safety |
| A2 | A lookup that sees `MemoryTier` sees an allocated and resident slot (the SSD read is complete). | Safety |
| A3 | Eviction never frees a pinned entry. | Safety |
| A4 | `mt_pool_used` equals the number of live slots. | Safety |
| A5 | At the end: no read reference is leaked, and the map and the memory tier agree. | Safety |
| **L1** | **After a serve from DRAM, the label is DRAM.** (fails on the code) | Safety |
| L2 | After the winner's own SSD read, the label is SSD. | Safety |

## Correspondence to the code (Certus `acd24025`, `components/dispatcher/src/lib.rs`)
| model step | code |
|---|---|
| classification `dm.lookup` (pins on a hit) | `lib.rs:2353-2445` |
| warm arm: label = DRAM | `lib.rs:2425` |
| cold arm: label = SSD, then the pin is released before the promotion | `lib.rs:2428-2435` |
| `mt.insert` decides the winner, the loser (`AlreadyExists`) or `PoolFull` | `lib.rs:1188-1217`, called from `2557-2581` |
| winner: SSD read, then `promote_block_to_memory_tier` | `lib.rs:2562-2566`, `2682` |
| loser: recovery pass, `serve_concurrently_promoted` (the label is NOT changed) | `lib.rs:2937-2945`, `1349-1386` |
| the label becomes the reported `served_by` | `lib.rs:2952-2956` |
| evictor | `evict_for_space` / `evict_one_clean` / `try_evict_to_block`: `lib.rs:1123`, `1050` |

## Returned values
| value | produced at | in the model |
|---|---|---|
| `dm.lookup`: `MemoryTier` / `BlockDevice` | dispatch-map `lookup` | the warm arm / the cold arm |
| `mt.insert`: `Ok` / `AlreadyExists` / `PoolFull` | `lib.rs:1188-1217` | `won` / `already` / `poolfull` |
| `served_by` label: `Dram` / `Ssd` | `lib.rs:2425`, `2435`, `2952-2956` | `label` (`L_DRAM` / `L_SSD`) — **the value that Daniel's model did not have** |
| `serve_concurrently_promoted`: `Ok` (served warm) | `lib.rs:1349-1386` | the recovery branch serves warm |

## Left out
- `dm.lookup`: `NotExist`, `MismatchSize`, and the `Err` retries (`lib.rs:2377-2386`); the `write_ref` wait and the
  5 s timeout (the model uses a limit on attempts instead).
- An SSD read failure (`lib.rs:2710`) and a failed `promote_block_to_memory_tier`. Root cause #11 is in that path;
  see `failed-promotion-slot`.
- The single-key `lookup_async` path, staging, the no-drives path, remote lookup, and multi-region serves.
- The model releases warm pins at once; the code keeps them until the batched sync (`lib.rs:2423`, `2754`).
