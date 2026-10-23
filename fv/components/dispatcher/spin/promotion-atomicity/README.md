# Cold-path Promotion Atomicity

> **What it checks:** a promotion from SSD to the memory tier is atomic when lookups and the SSD evictor run at the same time.
> **Result:** re-synced to the code: the model fails (errors: 1); the control copy passes.
> **The bugs:** root causes #31 (the SSD evictor frees a slot before the map removal), repaired in PR #555, and #11 (a failed promotion keeps its slot), repaired in PR #570 (open).
> **Certus commit:** `acd24025`.
> **History:** Daniel's model, re-synced to the code in J20 (Claude, 2026-10-17). `before_resync/` has Daniel's version and the diff.

## Scope

Whole system, single-key path: `IDispatcher::lookup` -> `lookup_async` -> `promote_and_serve`
-> DispatchMap / MemoryTier, racing the memory-tier demoter and the SSD-capacity evictor.

## Description

Checks that when a single-key `lookup` finds a key cold (BlockDevice in the dispatch-map)
and promotes it to the DRAM memory-tier, the promotion is atomic. That means:

- at most one thread holds the key's slot;
- a slot is freed only when nobody still uses it;
- a failed promoter leaves nothing behind;
- the caller gets a correct result.

The shipped promotion is **in place**. `promote_and_serve` (dispatcher `lib.rs:750-908`) takes
a slot with `evict_and_insert` (`mt.insert`) and reads the SSD into it. It then flips the entry
with `dm.promote_block_to_memory_tier` (dispatch-map `lib.rs:482-528`).

The memory-tier demoter (`evict_one_clean`, `lib.rs:1050-1108`; `MemoryTierEvictor`,
`background.rs:471-575`) calls `dm.try_evict_to_block`, then calls `mt.remove` as a separate
step. The SSD-capacity evictor (`BackgroundEvictor`, `background.rs:279-370`) calls `mt.remove`
before `dm.remove`.

v2 of this model encoded an older remove+recreate promotion
(`dm.remove` + `create_memory_tier_entry` + `release_write`). It also undid `mt.insert` on
failure. Neither exists on the shipped path (J20 sync). The `batch_lookup` in-place race is
covered by the sibling model `promotion-lookup-race`.

## Properties Verified

| ID | Property | Type | Where asserted |
|----|----------|------|----------------|
| P1 | At most one thread holds a live MT allocation per key; no `mt.remove` frees a slot an in-flight promoter still owns; the flip publishes the flipper's own slot | Safety | `mt.insert` branch, `mt_remove` inline, promote flip |
| P2 | `mt.insert(key)` succeeds at most once per key lifecycle, and the BlockDevice->MemoryTier flip happens at most once per lifecycle (`promotion_success[]` asserted) | Safety | `mt.insert` branch, promote flip, final check |
| P3 | After all threads complete: `read_ref = 0`, `write_ref = 0`, no promoter in flight | Safety | init final check |
| P4 | A promoter that returns `Err` leaves no slot of its own behind; at the end, slot exists iff entry is MemoryTier | Safety | after Err return, final check |
| P5 | Pool usage counter equals actual allocations | Safety | final check |
| P6 | (new) `lookup` of a key present in the dispatch-map never returns `AlreadyExists` (spec 001 US2 AS-2, `spec.md:61`; `contracts/errors.md:34`) | Safety | end of every lookup |
| P7 | (new) A MemoryTier hit is served from a live slot owned by the entry | Safety | MT-hit branch |
| P8 | (new) The slot holder's in-place promote finds its own BlockDevice entry (no `IoError("promote transition failed")` at 904) | Safety | after promote |

Also asserted on every lookup: the result is set, and `KeyNotFound` occurs only when
`dm.lookup` returned `NotExist`.

## System Abstraction

| Real component | Promela process / construct |
|----------------|-----------------------------|
| `IDispatcher::lookup` / `lookup_async` (single key) | `LookupThread(tid)` x N_THREADS, N_LOOKUPS calls each |
| `promote_and_serve` / `evict_and_insert` / `evict_for_space` | inline in `LookupThread` |
| `evict_one_clean` (also used by `MemoryTierEvictor::evictor_loop`, same try_evict -> mt.remove order) | `evict_one_clean` inline; `Evictor()` |
| `BackgroundEvictor::evictor_loop` (SSD capacity) | `SsdEvictor()` (`-DSSD_EVICTOR=0` to drop it) |
| `DispatchMapComponent::lookup` | atomic: wait write_ref==0, read_ref++, classify |
| `release_read` | atomic: read_ref-- |
| `promote_block_to_memory_tier` | atomic: BD -> MT in place, else InvalidState / KeyNotFound |
| `try_evict_to_block` | atomic: refs==0 && MT -> BD, else ActiveReferences / InvalidState / KeyNotFound |
| `dm.remove` | atomic: refs==0 -> EMPTY, else ActiveReferences |
| `MemoryTierComponent::insert` | atomic: AlreadyExists / PoolFull / Ok |
| `MemoryTierComponent::remove` | `mt_remove` inline |
| SSD read (pipelined NVMe + H2D) and channel checkout | nondeterministic Ok / Err |
| Staging serve (`AllocationFailed && has_drives`) | nondeterministic Ok / IoError / AllocationFailed (no buffer) |

## Assumptions / Stubs

- **SSD read / channel checkout**: either one can fail. Both fail after `mt.insert`, so they are one branch.
- **Staging pool**: when the tier cannot give a slot, the staged serve is abstracted to its three results. It never touches the slot or the entry.
- **LRU order** (`mt.oldest_keys`): abstracted. Any resident key may be inside or outside the scan window.
- **`max_eviction_attempts`**: bounded to `MAX_ATTEMPTS = 2` (default 2048, `idispatcher.rs:115`).
- **Condvar wait / Timeout**: `dm.lookup` blocks until `write_ref == 0`. Timeout is left out. No `write_ref` is taken on the modelled paths.
- **SSD evictor trigger**: runs only at SSD utilization >= `ssd_eviction_threshold` (0.9, `idispatcher.rs:110`). The model lets it run at any time.
- **Control hooks** (`FIX_RECOVER`, `FIX_UNDO`, `FIX_PIN`): all default to 0, which is the shipped code. Only `promotion_atomicity_control.pml` sets them. See the `.pml` header.

## Returned values

Every value the modelled API returns on the modelled paths (J20, acd24025):

| # | Value | Code line(s) | Modelled as | Checked by |
|---|-------|--------------|-------------|------------|
| M1 | `lookup` Ok, MemoryTier hit | lib.rs:3061 | `ret = R_OK` | P7, ret set |
| M2 | `lookup` Ok, promoted | lib.rs:907 -> 3067 | `ret = R_OK` | P1, P2, P8 |
| M3 | `lookup` Ok, staged (uncached) | lib.rs:893-896 | `ret = R_OK` | P4/P5 (no slot), ret set |
| M4 | `KeyNotFound` (NotExist) | lib.rs:3029 | `ret = R_KEY_NOT_FOUND` | KeyNotFound-only-if-NotExist |
| M5 | `AlreadyExists` | lib.rs:1217 -> 789 -> 3065 `?` -> 2198 `?` | `ret = R_ALREADY_EXISTS` | **P6** |
| M6 | `IoError`, SSD read / channel checkout after insert | lib.rs:883, 830 | `ret = R_IO_ERROR` | **P4** |
| M7 | `IoError("promote transition failed")` | lib.rs:903-904 | `pres != PB_OK -> R_IO_ERROR` | **P8**, P4 |
| M8 | `AllocationFailed` (no staging buffer) | lib.rs:781-785 | `ret = R_ALLOC_FAILED` | ret set, P5 |
| M9 | `dm.lookup` NotExist / BlockDevice / MemoryTier | dispatch-map lib.rs:143, 157, 158 | `lres` | P7, M4 check |
| M10 | `mt.insert` Ok / AlreadyExists / PoolFull | memory-tier lib.rs:389, 357, 363 | `ins` | P1, P2 |
| M11 | `evict_for_space` / `evict_and_insert` AllocationFailed | lib.rs:1149, 1161, 1193, 1202 | `alloc_failed` | P5 |
| M12 | `try_evict_to_block` Ok / ActiveReferences / InvalidState / KeyNotFound | dispatch-map lib.rs:569, 559, 573/576, 553 | `te` | P1 (mt_remove), P5 |
| M13 | `evict_one_clean` true / false | lib.rs:1071, 1107 | `freed` | P5 |
| M14 | `promote_block_to_memory_tier` Ok / InvalidState / KeyNotFound | dispatch-map lib.rs:527, 512, 496 | `pres` | P8 |
| M15 | `dm.remove` Ok / ActiveReferences / KeyNotFound (SSD evictor) | dispatch-map lib.rs:356, 342, 339 | branch in `SsdEvictor` | P3, P8 |
| M16 | `get_evictable_offset` Some / None | background.rs:391, 396/400/402 | `bd` | P1 |

### Left out

| # | Value | Code line(s) | Why |
|---|-------|--------------|-----|
| L1 | `NotInitialized` | lib.rs:2999, 3004, 3009, 3016, 2203, 853 | wiring / init, before or outside the race |
| L2 | `dm.lookup` Err (Timeout, RefCountOverflow) -> `KeyNotFound` | dispatch-map lib.rs:139, 150; lib.rs:3071 | no `write_ref` holder on modelled paths; refcount bounded |
| L3 | `MismatchSize` -> `InvalidParameter` | lib.rs:3030-3034 | `DispatchMapComponent::lookup` never returns it (dispatch-map lib.rs:143-162) |
| L4 | MT-hit serve error, integrity-check CRC `IoError` | lib.rs:3048-3061, 3040-3045 | GPU DMA failure, feature-gated CRC; no tier state change |
| L5 | `stream_synchronize` `IoError` | lib.rs:2205-2206 | `lookup_async` returns a null stream on every path (3061, 3067), so it is never called |
| L6 | No-drives branch: `IoError` 801/816, ignored `create_memory_tier_entry`/`release_write` results | lib.rs:795-818 | test-only mode (no data drives) |
| L7 | `mt.insert` InvalidSize / NotInitialized / PoolFull(377) -> `AllocationFailed` | memory-tier lib.rs:330, 335, 369, 377; lib.rs:1219-1221 | size 0 / init / unreachable after `can_allocate` |
| L8 | `promote_block_to_memory_tier` InvalidSize | dispatch-map lib.rs:489 | size 0 |
| L9 | `release_read` Err (KeyNotFound, RefCountUnderflow) | dispatch-map lib.rs:272, 275; ignored at lib.rs:3056, 3064, background.rs:390/395/399 | result discarded (`let _`) |
| L10 | `mt.remove` Ok / KeyNotFound | memory-tier lib.rs:513, 510; ignored at lib.rs:1068, background.rs:328, 535 | result discarded; modelled as no-op, value not asserted |
| L11 | tier counters / eviction events | lib.rs:905, 3059, 3066, 1216, 1069-1070, 1074, 1082, 1106 | telemetry; single-key `lookup` has no `served_by` label (that is `batch_lookup`, sibling model) |
| L12 | `em.remove_extent` result, extent reuse | background.rs:348 | extent manager not modelled (see RESULT.md, C3 consequence) |

### Summary

| Model | Status | Divergences | Verification | States | Unreached | Returned values |
|-------|--------|-------------|--------------|--------|-----------|-----------------|
| promotion-atomicity | UPDATED | remove+recreate -> in-place promote; no undo on Err; evict = try_evict_to_block then mt.remove; SSD evictor added; AlreadyExists propagation | **FAIL** (P6, candidate bugs C1-C3); control PASS | 4,289 (shipped, stops at 1st error); 2,800,356 (control) | control: 18 of 320 in LookupThread (inline evict_one_clean copy and the R_IO_ERROR-after-promote branch, unreachable by P8) | 16 / 12 / 0 |

## Running

```bash
make clean && make safety     # shipped code
make liveness
make clean && make safety MODEL=promotion_atomicity_control.pml   # minimal-fix control
spin -DSSD_EVICTOR=0 -a promotion_atomicity.pml                   # without SSD evictor
```

## Tuning the Model

| Parameter  | Value | Rationale |
|------------|-------|-----------|
| N_THREADS  | 3 | threads 0 and 2 race on key 0; thread 1 on key 1 |
| N_KEYS     | 2 | per-key isolation + cross-key eviction pressure |
| POOL_CAP   | 2 | `= N_KEYS`; `-DPOOL_CAP=1` exercises PoolFull / staging (control passes at 9.06M states) |
| N_LOOKUPS  | 1 | 2 gives >74M states for the control (8 GB MEMLIM hit, not completed) |
| MAX_ATTEMPTS | 2 | bounds the eviction retry loops |

## Correspondence to Source Code

All lines are re-derived by symbol at origin/unstable @ acd24025.

| Model location | Source file | Line range |
|----------------|-------------|------------|
| `LookupThread` / `IDispatcher::lookup` | `components/dispatcher/src/lib.rs` | 2197-2208 (`?` at 2198) |
| `LookupThread` / `lookup_async` | `components/dispatcher/src/lib.rs` | 2994-3073 |
| `LookupThread` / dm.lookup | `components/dispatcher/src/lib.rs` | 3011 |
| `LookupThread` / NotExist -> KeyNotFound | `components/dispatcher/src/lib.rs` | 3029 |
| `LookupThread` / MT hit serve + release_read | `components/dispatcher/src/lib.rs` | 3036-3062 (release_read 3056) |
| `LookupThread` / release_read before promote | `components/dispatcher/src/lib.rs` | 3064 |
| `LookupThread` / promote_and_serve `?` | `components/dispatcher/src/lib.rs` | 3065 |
| `LookupThread` / promote_and_serve | `components/dispatcher/src/lib.rs` | 750-908 |
| `LookupThread` / evict_and_insert call + Err propagation | `components/dispatcher/src/lib.rs` | 768, 789 |
| `LookupThread` / staging fallback | `components/dispatcher/src/lib.rs` | 770-786, 893-896 |
| `LookupThread` / channel checkout, SSD read Err | `components/dispatcher/src/lib.rs` | 830, 871-884 (883) |
| `LookupThread` / in-place promote | `components/dispatcher/src/lib.rs` | 903-907 |
| `LookupThread` / evict_and_insert | `components/dispatcher/src/lib.rs` | 1176-1224 |
| `LookupThread` / evict_for_space | `components/dispatcher/src/lib.rs` | 1123-1168 |
| `evict_one_clean` inline | `components/dispatcher/src/lib.rs` | 1050-1108 |
| `Evictor` / MemoryTierEvictor loop | `components/dispatcher/src/background.rs` | 471-575 (531, 535) |
| `SsdEvictor` / BackgroundEvictor loop | `components/dispatcher/src/background.rs` | 279-370 (323, 328, 331) |
| `SsdEvictor` / get_evictable_offset | `components/dispatcher/src/background.rs` | 384-405 |
| dm.lookup | `components/dispatch-map/src/lib.rs` | 128-174 |
| dm.release_read | `components/dispatch-map/src/lib.rs` | 267-285 |
| dm.remove | `components/dispatch-map/src/lib.rs` | 334-357 |
| dm.promote_block_to_memory_tier | `components/dispatch-map/src/lib.rs` | 482-528 |
| dm.try_evict_to_block | `components/dispatch-map/src/lib.rs` | 548-580 |
| mt.insert | `components/memory-tier/src/lib.rs` | 328-390 |
| mt.remove | `components/memory-tier/src/lib.rs` | 483-514 |
| control FIX_RECOVER / serve_concurrently_promoted | `components/dispatcher/src/lib.rs` | 1349-1388 (batch use 2937-2945) |
| interface: promote_block_to_memory_tier / try_evict_to_block | `components/interfaces/src/idispatch_map.rs` | 160, 186 |
