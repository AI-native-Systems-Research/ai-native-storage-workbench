# J23 — second new Spin model: `flush_to_ssd` then `clear_memory_tier` (data loss)

node7, 2026-10-20, UTC 05:43–06:20. Code: ai-native-storage-certus @ af221dd82 (current unstable, with the 7
merged PR #555 fixes), read with `git show`. No repository was edited. The method, references and memory cap
are the same as J22: workbench `tools-spin-model`, `~/FV/spin_references/`, `systemd-run … MemoryMax=60G`,
`-DMEMLIM=16000`, `-w24`. Spin 6.5.2.

Files in `~/FV/J23/` (branch `node7/J23-spin-flush-clear`):
- `flush_clear.pml` (317 lines) and `flush_clear_control.pml`;
- `Makefile`;
- `README.md`, which holds the skill's tables and the quoted contract;
- `*.log` for the four runs;
- `trails/`, with each trail and its `spin -t -p` replay.

## Headline
**Spin finds the data loss on today's code in 0.05 s and about 120 states.** All three asked-for properties fail,
each with its own trail. The control copy with a minimal fix passes safety and liveness.

| run | result | states stored | transitions | depth | pan time | memory | wall / max RSS |
|---|---|---|---|---|---|---|---|
| shipped, `make safety` | **FAIL**: first error found is (c) `flush_ret == nk` | 40 | 40 | 53 | 0 s | 138.9 MB | 1.9 s / 143 MB |
| shipped, `make liveness` | **FAIL**: same assertion, inside the L1 search | 40 | 40 | 93 | 0 s | 138.9 MB | 2.0 s / 143 MB |
| control, `make safety` | **PASS**, 0 errors | 1,522 | 2,681 | 91 | 0 s | 139.0 MB | 1.9 s / 143 MB |
| control, `make liveness` (L1, acceptance cycles, weak fairness) | **PASS**, 0 errors | 1,490 | 31,310 | 145 | 0.01 s | 139.0 MB | 2.3 s / 143 MB |

**Each property on the shipped code**, run alone with `-DONLY_<P>` so that it gets its own trail:

| property | shipped | control |
|---|---|---|
| (a) PA: after flush returns Ok, every stored entry has an SSD copy | **FAIL** (114 states) | pass |
| (b) PB: clear never removes the only copy of a stored entry | **FAIL** (122 states) | pass |
| (c) PC: flush's return value = entries with a valid SSD offset | **FAIL** (40 states) | pass |
| PF: final state has no leaked pin and no orphan slot | pass (1,920 states) | pass |
| L1: the client finishes | holds on the shipped code with assertions off (1,888 states) | pass |

## Which (a) the specification requires
The only text that defines `flush_to_ssd`'s guarantee is the interface doc, `idispatcher.rs:824-832`:
> "Guarantees that every entry populated before this call has its data persisted to the block device. After this
> returns, `clear_memory_tier` will convert all memory-tier entries to BlockDevice state rather than dropping
> them. Returns the number of entries that now have a valid SSD offset."

Dispatcher spec FR-056 (spec.md:344) adds that it "returns the number of entries flushed". FR-017 (spec.md:296)
says a failed write-through is "silently drop[ped] … (Known limitation)". FR-038 (spec.md:317) says clear removes
any entry it cannot transition. FR-040 is `promote_to_memory_tier` and says nothing about flush.

No text lets flush **report** a failed write-through. The specification requires the first form of (a): after
`flush_to_ssd` returns Ok, every stored entry has an SSD copy. I checked exactly that.

The control meets it the other way, by returning `Err` instead of Ok. That needs one added sentence in the doc
("…or returns `Err` if a write-through failed"). It is a spec change for the owners, which I flag but did not
make.

On (c): the interface doc says the return value is "the number of entries that now have a valid SSD offset",
while FR-056 says "the number of entries flushed". The two differ. I checked the interface doc's definition,
because it is the precise one. The shipped code satisfies neither (see below).

## The counterexamples in plain words
**Start.** The client calls `flush_to_ssd` only after both `populate` calls have returned Ok, so both keys
count as "stored before the flush". Each populate:
- reserves a slot (lib.rs:3194);
- copies from the GPU (3235-3246);
- creates the MemoryTier entry and downgrades its write ref to a read ref (3560, 3619);
- enqueues a write-through job, which increments `in_flight` (3628 → background.rs:78-81).

**(a) PA, flush says Ok while a stored entry has no SSD copy** (`trails/only_PA.trail`):
1. Key 1 is populated and its job queued. The writer takes it, the SSD write succeeds, `convert_to_storage`
   records the SSD offset (lib.rs:1469), and `in_flight` drops to 0 (background.rs:129).
2. Key 0 is populated and its job queued. The writer takes it, and `reserve_extent` or the SSD write
   **fails** (lib.rs:1446-1449 / 1458-1461). The writer only calls `release_read`: the job is silently dropped
   (FR-017), the entry stays MemoryTier without an SSD offset, and `in_flight` still drops to 0.
3. The client calls `flush_to_ssd`. It samples `in_flight = 0` (lib.rs:3899), `writer.flush()` returns at once
   because the queue is empty (3900; background.rs:198-205), and it returns `Ok(0)` (3906).
4. Key 0 was populated before the call and has no copy on the block device, which breaks the interface doc's
   "guarantees … persisted". → PA fails.

**(b) PB, clear destroys the only copy** (`trails/only_PB.trail`). Steps 1-3 are the same as for (a). Then the
client calls `clear_memory_tier`:
1. For key 0, `try_evict_to_block` returns **InvalidState**, because there is no SSD offset (dispatch-map
   571-578).
2. The else branch runs `mt.remove(0)` (lib.rs:3885), which frees the DRAM slot holding the **only** copy of
   key 0's data. → PB fails.
3. Next, `dm.remove(0)` (3886) deletes the entry.

The data is gone, although the caller flushed first and was told Ok. This is the bug Kani found on a mirror.
Loom held only because every SSD write succeeded in its setup, which matches the model: the failure branch of
the writer is what makes it reachable.

**(c) PC, flush returns the wrong number** (`trails/only_PC.trail`): both write-throughs succeed and both keys
have an SSD offset. `flush_to_ssd` returns `before`, which is `in_flight` sampled *before* the wait (3899,
3906), so it is 0 here. The interface doc says it returns the number of entries with a valid SSD offset, which
is 2. → PC fails.

The return value is the count of jobs still queued when flush began. That is neither "entries with an SSD
offset" (interface doc) nor really "entries flushed" (FR-056). It undercounts every job that finished before
the call, which is J21's DIS-FLUSH-RETURNS-COUNT.

**Holds:**
- PF: there is no leaked pin. The writer releases its pin on every exit, and convert_to_storage drops it.
- L1: the client always finishes.

## The minimal fix (control)
`flush_clear_control.pml` is `#define FIX 1` plus `#include "flush_clear.pml"`. No property, assert, actor or
bound changes, and `clear_memory_tier`'s body is unchanged. In Rust terms:
1. `process_write_job` (lib.rs:1395-1470) reports a failed job on its DmaBuffer, `reserve_extent` and
   SSD-write exits, instead of returning `()`. The worker records it.
2. `flush_to_ssd` (lib.rs:3894-3907): after `writer.flush()`, if any job failed, it returns
   `Err(DispatcherError::IoError(..))`. Otherwise it returns `Ok(number of entries with a valid SSD offset)`.
3. A caller that receives `Err` does not call `clear_memory_tier`. This is caller behaviour (modelled), not a
   code change.

With the fix, (a) holds in its "flush reports the failure" form, (b) holds because the clear never runs after a
failed flush, and (c) holds.

**Two things this fix does not do, for the owners to decide:**
- It does **not** make `clear_memory_tier` itself safe. A caller that ignores flush's error, or calls clear
  without flushing, still loses unpersisted data, which FR-038 explicitly allows today. A stronger fix would be
  for clear to refuse (or skip) entries with no SSD copy.
- It does **not** retry the failed write-through. The entry stays MemoryTier, unpersisted.

## What I modelled, and choices that could hide a bug
**Modelled:** two populates, the writer with both its success and failure branches, and the client doing flush
then clear, all concurrent. Returned values: 10 modelled, 5 left out, 0 missing. The value the bug turns on is
the write-through job's outcome, which the code drops (`process_write_job` returns `()`); it is modelled as the
writer's branch.

**Could hide a bug:**
- **Populates complete before the flush.** A populate racing the flush is not explored. Its job may be enqueued
  after `before` is sampled but before or after `flush()` returns, and that could break (a) differently. This
  is the main gap.
- **Bounds:** 2 keys, one populate each, one writer, one flush and one clear. `ParallelBackgroundWriter` with
  several per-drive writers is modelled as a single writer.
- **Populate always succeeds and caches.** Drop-on-full and the D2H, sync and completion failures are left out
  (README X3). Those entries are never "stored", so they do not weaken (a)/(b) as stated, but their
  interaction with flush is not explored.
- **No lookups or pins during the clear.** J22 covers that, and the bugs are independent.
- **The SSD copy is a ghost flag** (`on_ssd`). The extent manager is not modelled, so a published extent that
  is later freed is not seen (J20 B1).
- **Unreached code.** In the control, 2 of 63 BgWriter states and 12 Client states are unreached. They are the
  clear branches that cannot run once a failed flush skips the clear, plus error results that cannot occur
  here. The listing is in `flush_clear_control.safety.log`.

## Time per step (UTC, 2026-10-20)
| step | from → to | time |
|---|---|---|
| read the job, the code at af221dd82 (flush, clear, populate, process_write_job, writer) and the spec text | 05:43 → 05:46 | ~3 min |
| write the model, control and Makefile; parse check | 05:46 → 05:46 | (the model reuses the J22 structure; under 1 min after reading) |
| first runs (4) and per-property runs (4) with trails | 05:46 → 05:48 | ~2 min |
| re-derive the citations (2 were off: populate's `Ok(())` at 3258, and the no-writer branch at 3902-3904), final runs | 05:48 → 06:00 | ~12 min |
| README tables, the spec decision for (a), this report | 06:00 → 06:20 | ~20 min |

That totals about 37 minutes of the 3-hour box. There were no Spin setup errors this time, because the J22
lessons (no nested comments, NFAIR=3, `-DONLY_<P>` instead of `-e`) were applied from the start.
