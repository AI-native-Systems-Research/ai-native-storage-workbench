# flush_to_ssd then clear_memory_tier (data loss after a failed write-through)

**Scope**: Whole system. The actors are two `populate` calls, the background write-through worker, and one
client that calls `flush_to_ssd` and then `clear_memory_tier`. The dispatch map, memory tier and SSD that
they share are modelled as global state.

This model was written from scratch for node7 J23 (2026-10-19/20), using ai-native-storage-certus @
af221dd82, the current unstable, which already holds the 7 merged fixes of PR #555. The method is the
workbench `tools-spin-model` skill with its "Added in the workbench" rules. Every value the modelled API
returns is either modelled or listed under Left out, and every file:line was re-derived at af221dd82.

The model checks whether a caller that flushes before clearing can lose data. `flush_to_ssd` waits only
until the write-through queue is empty. A job that fails inside `process_write_job` still leaves the queue,
but the entry is left without an SSD copy. `clear_memory_tier` then force-removes that entry.

## What the specification requires (property (a))
- **Interface doc, `components/interfaces/src/idispatcher.rs:824-832`:** "Flush all pending background
  write-through jobs to SSD and block until complete. Guarantees that every entry populated before this call
  has its data persisted to the block device. After this returns, `clear_memory_tier` will convert all
  memory-tier entries to BlockDevice state rather than dropping them. Returns the number of entries that now
  have a valid SSD offset."
- **Dispatcher spec FR-056, `specs/001-dispatcher-cache-interface/spec.md:344`:** "`flush_to_ssd() -> usize`
  forces a synchronous write-through flush of pending dirty entries to SSD and returns the number of entries
  flushed (design invariant P9)."
- **FR-017 (spec.md:296):** "When the asynchronous background write-through fails (extent allocation failure
  or block device I/O error), the background writer silently drops the job. The dispatch map entry remains in
  MemoryTier state. (Known limitation.)"
- **FR-038 (spec.md:317):** clear_memory_tier transitions each entry to BlockDevice, and "If the transition
  fails (write-through not complete), the entry is removed from the dispatch map entirely."
- **FR-040 (spec.md:319)** is `promote_to_memory_tier`. It says nothing about flush and is not relevant here.

**Decision.** The only text that says what `flush_to_ssd` guarantees is the interface doc, and it says
"**guarantees** … persisted" with no failure case. Nothing in the spec allows `flush_to_ssd` to report a
failed write-through (it returns `Result<usize, _>`, but no requirement defines an error for that case).
FR-017 explains *why* the guarantee breaks, and calls it a known limitation of the writer, not of flush. So
(a) is checked as the interface doc states it: **after `flush_to_ssd` returns Ok, every entry stored before
the flush has an SSD copy.**

The control satisfies (a) in its other allowed form: flush **reports** the failure (`Err`) instead of
returning Ok. This is the minimal change. The doc should then add "or returns `Err` if a write-through
failed", which is a spec change for the owners to make.

## Properties Verified

| ID | Property | Type | Where |
|---|---|---|---|
| PA | (a) after `flush_to_ssd` returns Ok, every entry stored before the flush has an SSD copy | Safety | `Client`, at flush return |
| PB | (b) `clear_memory_tier` never removes the only copy of a stored entry, whether by freeing its DRAM slot or deleting its map entry while no SSD copy exists | Safety | `mt_remove`, `dm_remove` |
| PC | (c) `flush_to_ssd`'s returned value equals the number of entries that now have a valid SSD offset (interface doc l.831) | Safety | `Client`, at flush return |
| PF | final: no leaked pin, no orphan memory-tier slot | Safety | `init` |
| L1 | the client finishes: flush returns and clear returns (`<> client_done`) | Liveness (LTL, weak fairness) | `make liveness` |

## System Abstraction

| Real | Promela |
|---|---|
| `IDispatcher::populate` (2 client threads, keys 0 and 1) | `proctype Populator(k)` |
| `BackgroundWriter::worker_loop` + `process_write_job` | `proctype BgWriter`: per job, either success (SSD copy + `convert_to_storage`) or failure (`release_read` only) |
| `flush_to_ssd` then `clear_memory_tier` (one caller) | `proctype Client`, started once both populates have returned |
| the writer channel (unbounded) and the `in_flight` counter | `chan wq [N_KEYS]`, `byte in_flight` |
| durable SSD copy (extent published and data written) | ghost `on_ssd[k]` |
| "populate returned Ok with the entry cached" | ghost `stored[k]` |

## Assumptions / Stubs
- Every populate succeeds and caches its entry. Drop-on-full, D2H failures and sync failures are Left out
  (X3). The pool is assumed to hold N_KEYS slots.
- Both populates return before the flush starts, matching the property's phrase "stored before the flush".
  Populates that run concurrently with the flush are not explored.
- The writer's three failure exits (DmaBuffer, `reserve_extent`, SSD write) are one nondeterministic
  branch, because all three do the same thing to the modelled state (`release_read`, lib.rs:1437/1447/1459).
  The no-drives branch is Left out (X4).
- `flush` polls `in_flight == 0` every 5 ms. This is modelled as a blocking guard `(in_flight == 0)`.
- Each dispatch-map, memory-tier and queue call is one atomic step.

## Running
```bash
export PATH=$HOME/.local/bin:$PATH
make clean && make safety                       # memory-capped (systemd-run, MEMLIM, -w24)
make liveness                                   # L1, weak fairness, NFAIR=3
make safety   MODEL=flush_clear_control.pml
make liveness MODEL=flush_clear_control.pml
spin -a -DONLY_PB flush_clear.pml && cc -O2 -DMEMLIM=16000 -DSAFETY -o pan pan.c && ./pan -w24   # one property
```
`make clean` deletes `*.trail`; kept copies are in `trails/`.

## Tuning the Model
- `N_KEYS=2`.
- The control explores 1.5k states, so adding a third key, a second flush or concurrent populates would cost
  little. None of these were run (see the J23 report).

## Correspondence to Source Code (af221dd82, re-derived by symbol)

| Model | Source | Lines |
|---|---|---|
| `Populator`: reserve slot | dispatcher `lib.rs` `populate` → `reserve_memory` | 3194 |
| `Populator`: D2H + sync | lib.rs | 3235-3246 |
| `Populator`: create entry (write ref) | lib.rs `copy_gpu_to_memory_completed` → `dm.create_memory_tier_entry` | 3250 → 3560 |
| `Populator`: downgrade to read ref | lib.rs `dm.downgrade_reference` | 3619 |
| `Populator`: enqueue, in_flight++ | lib.rs `writer.enqueue` → `background.rs` `enqueue` | 3628 → 78-81 |
| `Populator`: returns Ok | lib.rs | 3258 |
| `BgWriter`: recv; in_flight-- after the job | background.rs `worker_loop` | 126, 129 |
| `BgWriter`: peek; None → release | lib.rs `process_write_job` | 1404; 1406-1408 |
| `BgWriter`: failure exits → release only (FR-017) | lib.rs (DmaBuffer, reserve_extent, SSD write) | 1436-1439, 1446-1449, 1458-1461 |
| `BgWriter`: success → SSD copy, publish, convert_to_storage | lib.rs | 1456, 1468, 1469 |
| `Client`: flush: sample `in_flight` | lib.rs `flush_to_ssd`; background.rs `in_flight` | 3899; 193-195 |
| `Client`: flush: wait for empty | lib.rs `writer.flush()`; background.rs `flush` | 3900; 198-205 |
| `Client`: flush returns `Ok(before)` | lib.rs | 3906 |
| `Client`: clear: candidates, break | lib.rs `clear_memory_tier` `mt.oldest_keys(64)` | 3876, 3877-3879 |
| `Client`: clear: try_evict; OK → mt.remove | lib.rs | 3881, 3882 |
| `Client`: clear: else → mt.remove + dm.remove (ignored) | lib.rs | 3885, 3886 |
| `Client`: clear: count, Ok(count) | lib.rs | 3888, 3891 |
| `dm_*` calls | `components/dispatch-map/src/lib.rs`: `release_read` 267-285, `convert_to_storage` 176-208, `remove` 334-357, `try_evict_to_block` 548-580 | |
| `mt_remove` | `components/memory-tier/src/lib.rs` `remove` | 483 |

## Returned values (J19 rule)

| # | Value | Produced at | In the model |
|---|---|---|---|
| M1 | `flush_to_ssd` → `Ok(usize)` | lib.rs 3906 | `flush_ret`, checked by PC |
| M2 | `clear_memory_tier` → `Ok(count)` | lib.rs 3891 | `count` (computed; its correctness is J22's PD, not re-asserted here) |
| M3 | `populate` → `Ok(())` | lib.rs 3258 | `stored[k] = true` |
| M4 | `dm.try_evict_to_block` → Ok / KeyNotFound / ActiveReferences / InvalidState | dispatch-map 548-580 | `OK` / `E_KNF` / `E_ACTIVE` / `E_INVALID` |
| M5 | `dm.remove` → Ok / KeyNotFound / ActiveReferences | dispatch-map 334-357 | `OK` / `E_KNF` / `E_ACTIVE` (ignored, as in the code) |
| M6 | `dm.convert_to_storage` → Ok / KeyNotFound / InvalidState | dispatch-map 176-208 | modelled (ignored, as in the code) |
| M7 | `dm.release_read` → Ok / KeyNotFound / RefCountUnderflow | dispatch-map 267-285 | modelled (ignored) |
| M8 | `mt.remove` → Ok / Err | memory-tier 483 | `OK` / `E_KNF` (ignored) |
| M9 | `mt.peek` → Some / None | memory-tier 423 | `peek_ok` |
| M10 | the write-through job's outcome (success / failure) | lib.rs 1395-1470 returns `()`: **the outcome is dropped** | modelled as the BgWriter branch; this is the value the bug turns on |

**Summary row:** 10 modelled / 5 left out / 0 missing.

## Left out

| # | What | file:line | What the model therefore cannot see |
|---|---|---|---|
| X1 | `ensure_initialized` / receptacle errors (`NotInitialized`) in populate, flush and clear | lib.rs 3179, 3895, 3862-3872 | early failures; they change no modelled state |
| X2 | `flush_to_ssd` with no writer (`bg_writer` is `None`) → `Ok(0)` | lib.rs 3902-3904 | a dispatcher built without a writer |
| X3 | populate's other outcomes: drop-on-full `Ok(())` without caching (3196-3210), D2H/sync errors with `release_memory` (3220-3246), `copy_gpu_to_memory_completed` errors (3541-3620) | lib.rs | stores that never reach the writer; they are not "stored", so (a)/(b) do not apply to them |
| X4 | the no-drives synthetic-offset branch | lib.rs 1412-1417 | memory-only mode; flush trivially "succeeds" there |
| X5 | `write_handle.publish()` result and the extent manager's internals | lib.rs 1468 | J20 B1/H1; a failed publish is not modelled (today it cannot fail) |

**Not modelled at all:**
- lookups and pins during the clear (that is J22);
- the SSD evictor and the memory-tier background evictor;
- `schedule_write_through`;
- populates concurrent with the flush;
- key reuse;
- `ParallelBackgroundWriter`'s multiple per-drive writers (flush sums them, background.rs:193-205; one
  writer is modelled).
