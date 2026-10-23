# failed cold promotion: is the reserved slot released, does the entry stay correct?

> **What it checks:** a promotion from SSD that fails: the reserved memory slot is released, and the key can be read again.
> **Result:** the model fails (P1, P3, P5); the control copy passes; 3 mutants fail as they must.
> **The bugs:** root cause #11 (a failed promotion keeps its memory slot), repaired by GLM-5.3, in PR #570 (open).
> **Certus commit:** `acd24025`.
> **History:** written from scratch in J25-B (GLM-5.3, 2026-10-21).

## Scope

Single component (dispatcher), the cold-lookup (BlockDevice → memory-tier)
promotion path. Two model files:

- `failed_promotion_slot.pml` — faithful model of the **current code**.
- `failed_promotion_slot_control.pml` — **control copy** with the minimal fix
  (release the reserved slot on failed SSD read, mirroring what
  `promote_to_memory_tier` already does at lib.rs:3770-3771), plus three
  mutant arms (`-DMUTANT_FLIP_BAD_SLOT`, `-DMUTANT_DOUBLE_FREE`,
  `-DMUTANT_DOUBLE_REMOVE`).

Actors (all modelled in both files):

| Real component | Promela process |
|----------------|-----------------|
| `lookup_async` → `promote_and_serve`, single key, whose SSD read FAILS (lib.rs:3063-3065, 750-908) | `LKP(1)` |
| A second lookup of the same cold key, healthy SSD — both single-key and batch `batch_lookup` pooled-cold behaviour are folded in (lib.rs:2228-2994, 2428-2443, 2553-2582, 2664-2714) | `LKP(2)` |
| The eviction path that frees memory: `MemoryTierEvictor` / `evict_one_clean` (background.rs:526-554; lib.rs:1050-1108) | `EV` (unbounded rounds) |
| End-state checks once both lookups returned | `FIN` |

Shared state: dispatch-map arrays `loc` (ABSENT/MT/BD), `persisted`, `rr`
(read pins); memory-tier arrays `slot`, `gen` (generation counter, ABA
detector), `slot_bad` (slot content came from a failed read).

## Properties Verified

| ID | Property | Type |
|----|----------|------|
| P1 | after every failed promotion, the reserved slot is released — checked by the failing lookup on its own reservation (generation `snap`; a concurrent re-insert is a different live allocation, not a leak) | Safety |
| P2 | no slot is released twice (the slots-map remove returns the slot once; a second remove is a KeyNotFound no-op — the raw-double-free mutant must be rejected) | Safety |
| P3 | after a failure the entry is still BlockDevice and can be read again — (i) at the failing return the entry is BD (or MT backed by a concurrent lookup's live slot); (ii) a lookup that STARTS after the failed promotion returned is served Ok | Safety |
| P4 | a lookup never returns Ok with data from a slot whose read failed (`promote_block_to_memory_tier` may only flip after a successful read) | Safety |
| P5 | at the end, no slot and no pin is leaked (rr==0 everywhere; every live slot belongs to an MT entry) | Safety |
| INV | a MemoryTier dispatch-map entry always points at a live memory-tier slot | Safety (invariant) |
| L | `no_permanent_orphan`: an orphaned slot is eventually freed or re-united with an MT entry (no permanent leak) | Liveness |

## Returned values (modelled ⟷ code)

| Operation | Modelled returns | Not modelled (Left out) |
|-----------|------------------|-------------------------|
| `dm.lookup` (dm lib.rs:128-174) | NotExist / MemoryTier{gen} / BlockDevice | Timeout, RefCountOverflow, MismatchSize, pointer/size values |
| `dm.release_read` (dm lib.rs:267-285) | Ok | KeyNotFound (pin is model-owned) |
| `mt.insert` via `evict_and_insert` (mt lib.rs:328-390; lib.rs:1176-1224) | Ok / AlreadyExists / AllocationFailed | InvalidSize, NotInitialized; pool slot identity |
| `mt.remove` (mt lib.rs:483-514) | Ok(free once) / Err(KeyNotFound no-op) | NotInitialized |
| `dm.try_evict_to_block` (dm lib.rs:548-580) | Ok(demote) / Err (pinned, unpersisted, wrong state collapsed) | which error variant |
| `dm.promote_block_to_memory_tier` (dm lib.rs:482-528) | Ok(in-place flip, SSD offset retained) | InvalidState arm (unreachable in-model) |
| `promote_and_serve` failed read (lib.rs:882-884) / batch failed job (lib.rs:2710) | `return Err(e)` — nothing released | error payload (Io etc.) |
| AlreadyExists recovery, single-key (lib.rs:789) vs batch (lib.rs:2937-2946 → 1349-1388) | single-key: `Err` (no recovery); batch: bounded wait then `Err(KeyNotFound)` | condvar wakeups, exact timeout |

## Left out

- Byte sizes, SSD offsets (retained implicitly by BD entries and by the flip),
  pointer values (replaced by generation counters).
- The remaining batch leak sites beyond the modelled one (create_stream
  failure lib.rs:2618-2627, channel checkout failure 2645-2651, no-drives
  promote failure 2497-2506): same shape as 2710, noted in REPORT.md by
  inspection.
- Staging fallback content (`serve_cold_staged`, lib.rs:919-1030) — reduced to
  its outcome (Ok, uncached).
- The multi-region post-flip serve window of the batch path (lib.rs:2682-2698):
  pinned-then-flipped serve is reduced to the flip + serve under a pin; the
  pinless multi-region window is noted by inspection in REPORT.md.
- Channels/pipeline DMA mechanics, GPU, CRC, `LOOKUP_RETRIES` exact count
  (bounded to 2 tries), condvar timing.
- Capacity: CAP=2 with 2 keys; the AllocationFailed → staging fallback path is
  modelled by its outcome only.
- Write path / `clear_memory_tier` (Part A covers them).
- 2 keys; the failing and healthy lookup target the same cold key 0.

## Specification basis

No spec for the promotion path exists in the snapshot (the skill's spec paths
under `components/*/specs/` are absent). The asserted properties come from the
task statement and the code's own reference behaviour:

- P1/P5 rest on the cleanup discipline the code itself implements in
  `promote_to_memory_tier` (lib.rs:3752-3778): on SSD-read failure it calls
  `mt.remove(entry.key)` (lib.rs:3771) before returning the error.
- P3 rest on the dispatch-map contract that a BlockDevice entry is always
  readable (`lookup`'s cold path, lib.rs:3063-3065).
- P4 rest on `promote_block_to_memory_tier` being called only after a
  successful read (lib.rs:903, 2681-2691).

**Spec gap (finding):** the pooled-cold promotion paths (single lib.rs:882-884,
batch lib.rs:2710 and siblings) do not follow the cleanup discipline their own
sibling `promote_to_memory_tier` documents and implements.

## Correspondence

| Model location | Intended behavior (spec / documented contract) | Implemented behavior (source) |
|---|---|---|
| `LKP` classify: `dm_lookup` → release pin before promoting | pin released at classify time | lib.rs:3063-3065 (single), 2428-2429 (batch) |
| `evict_and_insert` | evict_for_space then mt.insert; AlreadyExists checked before capacity | lib.rs:1176-1224; mt lib.rs:356-364 |
| `evict_one` (atomic demote+free) | try_evict_to_block then mt.remove, only unpinned persisted MT entries | dm lib.rs:548-580; lib.rs:1066-1071; background.rs:531-537 |
| `LKP(1)` failed read: `slot_bad[k]=1`, then straight to Err | **should be**: release the reserved slot (promote_to_memory_tier discipline, lib.rs:3771) | lib.rs:882-884 / 2710: `return Err(e)` with no `mt.remove` (**divergence — the finding**) |
| `LKP(2)` healthy read: `dm_promote_flip` after read | flip only after successful read, SSD offset retained | lib.rs:903 / 2681-2691; dm lib.rs:482-528 |
| `RES_ALREADY` single-key arm: `outcome_ok=0` | single-key path has no AlreadyExists recovery | lib.rs:789 (`Err(e) => return Err(e)`) |
| `RES_ALREADY` batch arm: bounded wait | recovery pass → serve_concurrently_promoted, else Err(KeyNotFound) | lib.rs:2937-2946 → 1349-1388 |
| `RES_ALLOC` arm: `outcome_ok=1` | AllocationFailed → staging fallback, served uncached Ok | lib.rs:770-788 / 2576-2578 |
| `EV` unbounded rounds while any slot exists | evictor keeps scanning; a vanished candidate is skipped | background.rs:526-554 |
| Control fix `mt_remove(k)` in failed branch | release the reservation before returning Err | mirrors lib.rs:3770-3771 |

## Mutant

- Known-bad build #1 = the **original** model (`make orig`): expected and
  observed violations of P1, P3, P5 and liveness (see REPORT.md).
- Known-bad build #2 = `make mutant-flip` (`-DMUTANT_FLIP_BAD_SLOT` on the
  control): a plausible WRONG fix — keep the slot and flip the entry anyway
  ("nothing leaked"). Expected and observed: P4 violated (`!(slot_bad[k])`,
  depth 43).
- Known-bad build #3 = `make mutant-dfree` (`-DMUTANT_DOUBLE_FREE`): a raw
  second deallocation bypassing the slots-map guard. Expected and observed:
  P2 violated (`slot[k]`, depth 44).
- Known-bad build #4 = `make mutant-drem` (`-DMUTANT_DOUBLE_REMOVE`): the
  fix's `mt.remove` called twice. Observed: **also rejected** (invariant
  `loc!=MT || slot`, depth 48) — `mt.remove` is keyed by CacheKey, not slot
  handle, so with a concurrent re-insert the second remove frees the NEW live
  slot (dangling MT entry). The fix must remove exactly once.
- Fixed build (`make control`): 0 errors, safety and liveness.

## Running

```sh
make control        # fixed build: 0 errors (safety 11,885 states; liveness 0)
make orig           # current code: P1 violated (depth 43)
make prop-p1 prop-p3 prop-p5   # attribution: P3 depth 48, P5 depth 46
make prop-p2        # = prop-p4: P2 and P4 hold (0 errors, 8,071 states)
make mutant-flip mutant-dfree mutant-drem   # all rejected
make liveness-orig  # acceptance cycle (depth 97): permanent orphan
make liveness-control              # 0 errors
# trail replay (same -D flags as the generating run):
spin -t -p -g -l failed_promotion_slot.pml
spin -DNO_P1 -DNO_P5 -t -p -g -l failed_promotion_slot.pml   # P3 trail
```

Preserved counterexample trails: `trail_b_original_p1/p3/p5/liveness.pml.trail`,
`trail_b_control_MUTANT_*.pml.trail`.

Every `pan` run goes through:
`systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p OOMPolicy=continue timeout 1800 ./pan… -m100000`
and `pan` is compiled with `-DMEMLIM=16000`.

## Tuning the Model

- `NKEYS` (2) / `CAP` (2): the smallest space that still exercises eviction
  pressure, the insert race, and re-insertion after a leak. Raising either
  multiplies the state space; every counterexample here needs only key 0.
- `tries` bound in the batch AlreadyExists arm (2): models
  `serve_concurrently_promoted`'s bounded wait; raising it only lengthens the
  timeout traces.
- `gen` is a byte and never wraps in these bounds (max 4).
