# J20 — write-before-evict: RESULT

Source: `origin/unstable` @ `acd24025` (read-only export `/tmp/j20src/components/`).
Method: `~/FV/J20/tools-spin-sync.SKILL.md` including the "Added in the workbench" parts.
Spin 6.5.2. Paths below are relative to `components/`. `lib.rs` = `dispatcher/src/lib.rs`,
`bg.rs` = `dispatcher/src/background.rs`.

**Sync status:** MODEL UPDATE REQUIRED → updated. **Verification: FAIL** (safety and liveness,
the same assertion). **Candidate bug: YES** (finding 1, P5). There is also a second, smaller
finding (P7, a counter label, finding 2). The control copy with both one-line fixes passes.

## 1. What changed vs `~/FV/J20/orig/write-before-evict` (by item)

I checked each J18 claim against the code. All of them hold.

1. **Blind-LRU path removed (removed path, 2d.3).** v3 `evict_one()` (orig .pml 61-159) freed
   the slot first, then did `convert_memory_tier_to_block`, else `dm.remove`, and had
   `LOST`/`EVICTED_PENDING` states. Today `evict_one_clean` (lib.rs:1050-1108) only calls
   `dm.try_evict_to_block` (1066), then `mt.remove` (1068) on Ok. It skips `ActiveReferences`
   (1073-1075) and every other `Err` (1076-1083), then returns false (1106-1107). It never
   blind-frees and never calls `dm.remove` (spec 001 FR-024 l.301, edge case l.234). In v4,
   `evict_one_clean()` is one atomic `try_evict_to_block` (dispatch-map/src/lib.rs:548-580) that
   returns all five results, followed by a separate `mt_remove`. `LOST`/`EVICTED_PENDING` and
   the SSD evictor's EVICTED_PENDING cleanup are deleted. The README keeps them as legacy notes.
2. **The populate failure path changed (semantic, 2d.4).** v3 retried evict forever. Today, when
   nothing can be evicted, the code returns `AllocationFailed` (lib.rs:1157-1163,
   1193-1206), retries under bounded backpressure (lib.rs:3400-3418), and then drops the store
   with `Ok(())` (lib.rs:3161-3174, FR-060). This is modelled with `MAX_ATTEMPTS`/`BP_RETRIES` and `ret`.
3. **mt.insert and dm entry creation are now separate steps (structural).** v3 inserted both in
   one atomic step. Today the order is `mt.insert` (lib.rs:1188), then the D2H copy
   (`mt.get` 3435, sync 3192-3194), then `mt.get` (3506), then `create_memory_tier_entry`
   (3508-3518, with `mt.remove` on AlreadyExists 3511), then `downgrade_reference` (3552), then
   `enqueue` (3561). v4 models each of these as its own step.
4. **Writer channel is unbounded (semantic).** v3 had `QUEUE_CAP=1` with backpressure. Today the
   channel is `crossbeam_channel::unbounded()` (bg.rs:51-52). v4 sets QUEUE_CAP=4, the total
   number of populates, so a send never blocks.
5. **Writer has no generation guard (key_gen: no code counterpart).** `process_write_job` only
   does `mt.peek(job.key)` (lib.rs:1399), which is keyed. v4 drops the 6-way generation `if`
   (orig 230-285) and models today's paths: peek None → release_read (1399-1405);
   reserve_extent Err or write Err → release_read (1439-1444, 1451-1456; merged into one branch
   because the effect is the same); publish + `convert_to_storage` (1463-1464, which sets
   `ssd_offset` and drops the ref, dispatch-map/src/lib.rs:176-208). `key_gen` is re-purposed
   to mean DRAM-slot identity: the `*mut u8` returned by `mt.insert`.
6. **The SSD evictor matches today's code (structural).** v3 cleared ON_SSD atomically. Today
   the code checks `get_evictable_offset` (bg.rs:322, 384-404), then calls `mt.remove(key)`
   (bg.rs:327-328), then `dm.remove(key)` (bg.rs:331-333), then `remove_extent` (345-349),
   in three separate steps.
7. **Background `MemoryTierEvictor` (bg.rs:471-576)** is not a separate process. It uses the
   same `try_evict_to_block` (531) → `mt.remove` (535) order as `evict_one_clean`, which every
   Client already runs. It is also off by default (`memory_tier_eviction_threshold: 0.0`,
   interfaces/src/idispatcher.rs:120). This is listed in the README.
8. **Properties.** P1, P2 and P4 are kept. P3 is kept with a new wording: "no MemoryTier entry
   resolves to a freed slot". The old wording "LOST → !in_dm" is legacy. New: **P5** (slot not
   freed while in use, FR-024), **P6** (populate returns only Ok/Ok-dropped/AlreadyExists,
   FR-003/FR-060/US1 AS3), **P7** (FR-033 counter label, spec 002 l.566-583).
9. **Params.** The orig README (POOL_CAP=3, QUEUE_CAP=4, P1-P3) disagreed with the orig .pml
   (2/1, P1-P4). Both are replaced: N_CLIENTS=2, N_KEYS=3, POOL_CAP=2, QUEUE_CAP=4,
   MAX_ATTEMPTS=2, BP_RETRIES=1. Key 0 is populated twice.
10. **Citations.** Every file:line in the README Correspondence table, the .pml header and the
    inline comments was re-derived by symbol at acd24025. The orig citations
    (lib.rs 296-348, 350-431, 1399-1498, bg.rs 76-93, 158-239) all pointed at stale or
    unrelated code.
11. **Makefile:** unchanged.

## 2. Returned values

| Model | Status | Divergences | Verification | States (control) | Unreached (control) | Returned values |
|-------|--------|-------------|--------------|------------------|---------------------|-----------------|
| write-before-evict | UPDATED | blind-LRU removed; drop-on-full; split insert/create; unbounded queue; no writer guard; SSD evictor `mt.remove` | **FAIL** (P5); control PASS | 8,617,412 | 7 (`else->skip` / defensive branches, see §3) | **18 / 9 / 0** |

The full table is in README "Returned values" (18 modelled) and "Left out" (9 entries, each
with a file:line). Missing = 0.

## 3. Results (`make clean && make safety`, `make liveness`; `/usr/bin/time -v`)

| Run | errors | states stored | transitions | depth | pan elapsed | total mem (MB) | wall | max RSS |
|-----|--------|---------------|-------------|-------|-------------|----------------|------|---------|
| `make safety` (today's code) | **1** — `assert(filling[x] != mt_slot[x])` .pml:124 (P5), at depth 140 | 30,333 | 52,091 | 250 | 0.02 s | 135.977 | 1.96 s (includes spin+cc) | 140,132 KB |
| `make liveness` (today's code) | **1** — same assert, same trail | 30,333 | 52,091 | 250 | 0.02 s | 135.977 | 1.70 s | 140,132 KB |
| control `make … safety` (both fixes) | 0 | 8,617,412 | 14,630,865 | 285 | 4.97 s | 983.536 | 6.27 s | 1,007,980 KB |
| control `make … liveness` | 0 | 8,617,412 | 14,630,865 | 285 | 4.26 s | 920.059 | 6.31 s | 942,948 KB |
| P7 isolated (`-DFIX_SSD_EVICTOR`, safety) | **1** — .pml:182 (P7), at depth 58 | 141,065 | 247,565 | 271 | 0.08 s | 146.524 | 0.10 s (pan only) | 150,892 KB |

Unreached states are not reported for the failing runs, because pan stops at the first error.
Control unreached (identical .pml lines in both control runs):
- Client .pml:132 ×2: the `KeyNotFound` `else->skip` branch of `mt_remove`, which no caller reaches.
- Client .pml:346, 348: the `else->skip` after `R_KEY_NOT_FOUND`. Expected: with the fix,
  `mt.get` never returns None.
- BgWriter .pml:383: peek None → release_read. With the fix, a pinned job's slot is never freed.
- BgWriter .pml:413: the `convert_to_storage` ref==0 branch, which is defensive.
- SsdEvictor .pml:463: `dm.remove` refused.

None of these is a modelled code path that should be live. Each one is unreachable *because*
the protocol is correct.

Liveness: no LTL claim exists (as in v3). `-a` therefore checks invalid end states and asserts
only ("acceptance cycles - not selected").

Logs: `make_safety.log`, `make_liveness.log`, `control_safety.log`, `control_liveness.log`,
`bug2_fr033_safety.log`.

## 4. Candidate bug 1 (P5): the SSD evictor frees a re-populated DRAM slot

Trail: `write_before_evict.safety.trail` (= `write_before_evict.liveness.trail`).
Replay: `spin -t -p -g -k write_before_evict.safety.trail write_before_evict.pml`, saved to
`trail_safety_replay.txt`.

Steps in plain words. Trail step numbers are on the left; code is at acd24025.

1. (11-32) Client 1 populates **key 2**: `mt.insert` (lib.rs:1188), D2H copy
   (3192-3194), `create_memory_tier_entry` (3508), `downgrade_reference` (3552, read_ref=1),
   `enqueue` (3561).
2. (33-57) Client 1 populates **key 0** the same way. The pool is now full (2/2).
3. (61-87) Client 0 populates key 1. `evict_for_space` (1134) → `evict_one_clean`
   (1056-1083): both candidates are pinned by write-through, so the result is
   `ActiveReferences` and both are skipped. Then `AllocationFailed` (1157-1163), then one
   backpressure retry (3401-3416).
4. (91-111) The writer processes both jobs. Key 2: reserve_extent/write error →
   `release_read` (1439-1444 / 1451-1456), so key 2 stays MemoryTier without `ssd_offset`.
   Key 0: write OK → `publish` + `convert_to_storage` (1463-1464), so key 0 has `ssd_offset`
   and read_ref=0.
5. (113-118) Client 0's retry: `evict_one_clean` → `dm.try_evict_to_block(0)` = Ok, so key 0 is
   **BlockDevice** (dispatch-map/src/lib.rs:563-567). Then `mt.remove(0)` (lib.rs:1068) frees
   key 0's slot. This is a correct demotion.
6. (121-136) Client 0's **second populate is key 0 again** (the block was evicted from DRAM and
   is stored again, which is the normal re-store case). The dispatch-map still has key 0 as
   BlockDevice. `mt.insert(0)` (lib.rs:1188; memory-tier/src/lib.rs:356 sees no slot) succeeds
   with a **new** slot. Client 0 is now in `copy_gpu_to_memory_async` / `copy_gpu_to_memory_completed`
   (lib.rs:3435-3506), copying GPU data into that slot.
7. (139-141) The **SSD evictor** runs with utilization ≥ threshold (bg.rs:296-305). It picks
   key 0 from `dm.oldest_keys` (bg.rs:314). `get_evictable_offset` (bg.rs:322, 388-392) sees
   **BlockDevice**, so the candidate is evictable. Then `let _ = mt.remove(key)` "no-op if not
   present" (**bg.rs:327-328**) runs. It is not a no-op: memory-tier `remove`
   (memory-tier/src/lib.rs:483-514) is keyed and frees **the new slot that Client 0 is filling**.
   **P5 fails here** (.pml:124).
8. After this, the model with P5/P6 disabled (variant run, 30,925 states, 0.02 s) continues
   as follows. `dm.remove(0)` succeeds (bg.rs:331, refs 0). Client 0's
   `create_memory_tier_entry` then publishes a MemoryTier entry pointing at the freed slot, so
   **P3 fails** (.pml:321). The next `mt.insert` of any key may receive that DRAM, and a later
   `lookup` of key 0 serves another key's bytes. On the other branch, `mt.get` (lib.rs:3435 or
   3506) returns None, and populate returns **`Err(KeyNotFound)`**. FR-003/FR-060 do not allow
   that error, and the vLLM connector treats it as fatal (spec 001 l.227). That branch is P6.

**Why it is reachable in the real code.** Nothing serializes populate against the SSD evictor
thread. The evictor's `get_evictable_offset` → `mt.remove` → `dm.remove` (bg.rs:322-333) are
three separate lock acquisitions. The memory-tier slot for a key and the dispatch-map entry for
that key are created at different times (lib.rs:1188 vs 3508). So "dispatch-map says
BlockDevice" does not imply "the memory-tier has no slot for this key". The comment at bg.rs:327
and spec 001 l.236 ("No memory-tier operation is needed (the entry was already evicted from
DRAM)") assume exactly that implication. The evictor runs by default (`ssd_eviction_threshold:
0.9`, interfaces/src/idispatcher.rs:110) whenever data drives exist (lib.rs:2053), and it fires
under SSD pressure, which is when re-stores of evicted blocks happen. The race window is the
whole D2H copy plus the dispatch-map registration.

**Minimal fix (control `FIX_SSD_EVICTOR`).** Delete `let _ = mt.remove(key);` at
bg.rs:327-328. Spec 001 l.236 already says no memory-tier operation is needed. A
BlockDevice entry never owns a slot, because `try_evict_to_block` + `mt.remove` (lib.rs:1066-1068,
bg.rs:531-535) free it at demotion. With this fix (and fix 2), the control has 0 errors in
8.6M states. An alternative fix is to make `remove` pointer-checked (`remove_if(key, ptr)`),
but that touches the interface.

**Not checked.** I expect that the cold-promotion path (`batch_lookup` → `evict_and_insert`
lib.rs:2558 → `promote_block_to_memory_tier` 2682) has the same exposure: it `mt.insert`s a key
whose dispatch-map entry is BlockDevice. `batch_lookup` drops its own lookup read-ref at
classification (lib.rs:2429), so whether `dm.remove` is refused there depends on a caller pin
(`pin()`, lib.rs:3594). The evictor's `mt.remove` (bg.rs:328) runs before `dm.remove` either
way, so it could free the slot being DMA'd into. I have not verified this: the path is not
modelled, and it is listed in README "Left out".

## 5. Finding 2 (P7, spec 002 FR-033): the "unpersisted" counter catches every other refusal

Trail: `write_before_evict.bug2_fr033.trail`. It was produced with `-DFIX_SSD_EVICTOR`, so
finding 1 is masked. Replay with
`spin -DFIX_SSD_EVICTOR -t -p -g -k write_before_evict.bug2_fr033.trail write_before_evict.pml`,
saved to `trail_bug2_replay.txt`.

1. (11-49) Client 1 populates key 2 and key 0. Key 0 has done `mt.insert` (lib.rs:1188) but has
   not yet reached `create_memory_tier_entry` (3508).
2. (51-59) Client 0 needs space and runs `evict_one_clean`. `mt.oldest_keys` (1056) returns key 0
   (memory-tier `insert` tracks the key at memory-tier/src/lib.rs:366). `try_evict_to_block(0)`
   returns **`KeyNotFound`** (dispatch-map/src/lib.rs:550-553), which matches `Err(_)` (lib.rs:1076).
   The code calls `record_eviction_blocked_unpersisted()` (1082). That violates FR-033: the
   counter "MUST be counted … when it refuses for want of an ssd_offset", and the comment at
   1077-1081 says the counter is there so "a third cause … shows up". This trail *is* the third
   cause.
   The same mislabelling happens for `InvalidState("entry is not in memory-tier state")`
   (dispatch-map 576-578; checked separately, 1 error).

Impact: telemetry only (`evictions_blocked_unpersisted` over-counts). Data is unaffected.
Minimal fix (control `FIX_FR033_LABEL`): match
`Err(DispatchMapError::InvalidState(_))` for the unpersisted counter and do not count
`KeyNotFound`. Better still, use a dedicated variant for the no-ssd_offset case, because
`InvalidState` also covers "not MemoryTier". The assert is not weakened: the same P7 assert is
in both the model and the control, and only the counting rule differs.

## 6. Honest notes / not checked

- The `.pml` assert on line 124 (P5) is a ghost check (`filling[]`). I believe it is right:
  FR-024 says eviction "NEVER frees a slot" that is in use, and the D2H target is in use
  (lib.rs:3469-3474 writes into it).
- Abstractions: the SSD evictor's "utilization ≥ threshold" check is replaced by "may run at any
  time". LRU order is replaced by index order. `oldest_keys` window widening (lib.rs:1156) is not
  modelled: the model scans all keys. Shard fragmentation (PoolFull after `evict_for_space` OK)
  is reachable only through the `attempts` counter. Lookup/pin/touch/remove(key) client calls are
  not modelled. `batch_populate` (lib.rs:3211) is not modelled (it uses the same
  `reserve_memory` and the same drop-on-full).
- Parameters are small (3 keys, 2 clients, 4 populates). The control ran in 8.6M states, < 10M.
- I did not run git, touch the repo, or edit `~/FV/J20/orig/`. The `control/` and `bug2/`
  subdirs hold copies used for the runs.
