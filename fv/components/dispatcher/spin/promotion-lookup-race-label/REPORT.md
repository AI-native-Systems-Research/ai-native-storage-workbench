# J18 — Spin vs Loom: what do Daniel's 8 Promela models cover, and does one find the Loom bug?

node7, 2026-10-16/17 (UTC 05:58–06:20). Source: `origin/unstable` @ acd24025. We exported it to /tmp/j18 with `git archive`. We did not edit, commit or branch anything in the repository. Spin 6.5.2 (~/.local/bin). Artifacts: `~/FV/spin_vs_loom/`.

**Headline:**
- Daniel's `promotion-lookup-race` model has no served-by label, so it cannot see the Loom bug.
- We added the label to a /tmp copy (+9 lines). With the label, Spin **finds the bug in 0.01 s**: 1 error, 8,158 states, 139 MB.
- The same copy with the fix passes: 0 errors, 58,970 states.
- The original model still passes: 0 errors, 39,626 states.
- Step 1 found a second, broader problem. Every README cites stale line numbers. 4 of the 8 models check a protocol that the current code no longer has, either fully or in 1 branch:
  - `promotion-atomicity` (remove+recreate);
  - `write-before-evict` (blind-LRU evict);
  - `populate-lookup-linearizability` (the eviction drop branch);
  - `no-lost-extents` (prepare/commit/cancel_store).

So the result "all 8 pass" proves less than it seems to prove.

## 1. What each model covers (code + specification, with file:line) and what it leaves out

2 agents wrote these paragraphs independently, 1 agent for models 1–4 and 1 agent for models 5–8. Each agent checked every citation against `origin/unstable` @ acd24025. Agent 1 also ran its 4 models again.


### check-pin-eviction-race
**Code that the model covers.**
- `evict_one_clean` (disp:1050–1106). The model has 2 branches.
  - The demote branch matches `dm.try_evict_to_block` → `mt.remove` (disp:1066–1071; dm:548–580).
  - The *fixed* build skips a victim that is not persisted (disp:1077–1101). The `-DBUGGY_DROP_FALLBACK` arm puts back the `dm.remove` fallback that commit `1d55b9c2` deleted. That commit is an ancestor of `origin/unstable`.
- The populate path:
  - `populate` (disp:3143) → `reserve_memory` (disp:3318) → `evict_and_insert` (disp:1176) → `evict_for_space` (disp:1123). The model collapses these into a single `MAX_TRIES` loop.
  - The model shows `copy_gpu_to_memory_completed` (disp:3489) as 1 atomic step: `create_memory_tier_entry` (disp:3508; dm:393), then `downgrade_reference` (disp:3552; dm:307), then `writer.enqueue` (disp:3561).
- The write-through: `process_write_job` (disp:1390). On success, the code calls `convert_to_storage`, which sets `ssd_offset` and drops the read-ref (disp:1464; dm:176). On failure, the code calls `release_read` (disp:1402/1432/1442/1454).
- The Connector Load is `dm.lookup` with a pin on hit (dm:128).

**Stale citations.** Every `lib.rs` line in the README and in the `.pml` header is stale:
- 2860 → `populate` is at 3143
- 2907 → `reserve_memory` is at 3318
- 1015 → `evict_and_insert` is at 1176
- 962 → `evict_for_space` is at 1123
- 920 → `evict_one_clean` is at 1050
- 3025/3044/3088 → 3489/3508/3552
- 2160–2214 → `batch_lookup` is at 2228
- 3676 → `dm.lookup` is at 3011 in `lookup_async`, and at 2379 in `batch_lookup`
- 3730/3933 → there is no `convert_to_storage` call there; the calls are at disp:1410/1464.

The doc comment on `evict_one_clean` (disp:1038–1039) still says that it "drop[s] it via `dm.remove`". That comment contradicts the fixed code body.

**Specification.** Dispatcher `specs/001-dispatcher-cache-interface/spec.md` FR-024 (l.301) and the edge case at l.234 (skip victims that are not persisted, never drop them). Also FR-017 (l.294: a failed write-through stays MemoryTier) and FR-060 (l.356: AllocationFailed degrades). Dispatch-map specification FR-004 (l.245: lookup pins), FR-010 (l.251), FR-011 (l.252) and FR-026 (l.267).

**Properties.**
- R-RESOLVE is `assert(loaded_ok)` (pml:347). It holds in the fixed build. In the buggy build, Spin finds a violation at depth 172 after 23,775 states.
- R-REF is pml:161.
- The BgWriter job-validity asserts are pml:282–283, and the pin-held assert is pml:352–354.
- R-FIN is pml:422/424/433: no leaked refs, and a pool slot implies a present MT entry with `pool_used == live`.

**Bounds.** `N_KEYS=2`, `POOL_CAP=1`, `N_CONN=2`, `MAX_TRIES=4`. Processes: 2 Populators (1 populate per key), 2 Connectors (1 Check, then 1 Load each), 1 BgWriter and 1 Evictor. The fixed build gives 692,978 states, depth 207, 0 errors. Exactly 1 state is not reached, `loaded_ok = 0` at pml:336, and that is intended.

**Left out.**
- **The model gives the wrong label to the real `populate` return value.** On `AllocationFailed`, the real `populate` returns `Ok(())` (drop-on-full, disp:3161–3174). The model treats this as "serves uncached" and sets no `pop_ok`.
- **Timed retries.** The model reduces the `reserve_memory` 20 ms backpressure loop with its deadline (disp:3399–3418) to a count.
- **Scan order.** The model scans by key index. The real code scans `oldest_keys(scan)` in LRU order, and it makes the scan wider by `MAX_SCAN×attempts`. The model also does not have the `evict_and_insert` PoolFull/fragmentation retry or its `AlreadyExists` arm.
- **Check.** In the model, Check is a plain read with no pin. The dispatcher's own `check` (disp:3075) takes and releases a transient read-ref, and it maps a `Timeout` to `Ok(false)` (disp:3092).
- **Load errors.** The model does not include these outcomes: `Timeout` → `KeyNotFound` (disp:3071; `batch_lookup` retries 5× at 2379–2386, then gives `KeyNotFound` at 2446), `MismatchSize`, the integrity-CRC failure, the `promote_and_serve` failures, and the remote-forward itself (disp:2757).
- **Removers.** Legitimate removers are out of scope: explicit `remove` (disp:3119), the force `dm.remove` in `clear_memory_tier` (disp:3819), and the `dm.remove` of the SSD evictor (background.rs:331). So the model claims R-RESOLVE only when these removers are absent. The model folds `MemoryTierEvictor` (background.rs:475/531) into the shared `evict_one_clean`.
- **Populate errors and other paths.** The model does not include: the D2H, `AlreadyExists` and `downgrade` error paths in populate, `batch_populate`, the write-ref window between create and downgrade, and the generation/key-reuse check. (Each key commits at most 1 time, so `gen` never becomes stale.) The model bounds `write_q` to `N_KEYS`, but the real channel is unbounded and per-drive.

### extent-bitmap-consistency
**Code that the model covers.** 1 `Slab` with its `AllocationBitmap`, behind a `RegionState` write lock (`atomic{}`):
- `Slab::alloc_slot`, `free_slot` and `set_key` (`extent-manager/src/slab.rs:29–35/37–40/42–44`)
- `AllocationBitmap::set`, `clear` and `find_free_from` (`bitmap.rs:17–24/26–33/42–51`)
- `RegionState::publish_slot` (`region.rs:114–119`) and `remove_extent_by_offset` (`region.rs:121–150`)
- `reserve_extent` (`lib.rs:584–630`: `alloc_extent` under `region.write()` at 588–589, `publish_fn` at 597–616, `abort_fn` at 618–621). The model reaches it through `WriteHandle::publish`, `abort` and `Drop` (`interfaces/src/iextent_manager.rs:132–139/141–146/149–155`)
- `remove_extent` (`lib.rs:680–684`)

**Stale citations.** The README's `region.rs 117–122` must be 114–119, and `region.rs 124–153` must be 121–150. `lib.rs 516–528/533–549` is stale: 514 is `initialize`, and reserve/publish/abort are at 584–621. The slab.rs and bitmap.rs ranges are approximately correct.

**Specification.** `extent-manager/specs/001-extent-manager-v2/spec.md` FR-005 (l.324), FR-006 (l.328), FR-007 (l.332), FR-008 (l.334), FR-010/FR-011 (l.349/353), FR-020 (l.404, bitmap + rover), FR-023 (l.417, per-region lock), FR-025 (l.427), FR-033 (l.493) and SC-004 (l.572).

**Properties.**
- P2 (pml:91) is vacuous, because the model asserts it inside the `!bitmap[idx]` guard. The real check is only a `debug_assert!` (bitmap.rs:19/28).
- P3 (pml:112), the publish precondition (pml:124) and handle-ownership (pml:180/188) are real asserts.
- The model checks P1 (pml:291) only at the final quiescent state. It checks free-slot cleanliness (pml:294–295) at the same point.
- P4 has no assert. It is only the scan guard at pml:212–213.
- P5 is `allocated_count == set bits` (pml:303).

**Bounds.** `N_THREADS=3`, `N_SLOTS=4`, `OPS_PER_THREAD=2`. Each op is reserve, then publish-or-abort, then an optional remove. Result: 18,995,828 states, depth 181, 0 errors, 0 not reached.

**Left out.**
- **Deferred free.** In the model, remove frees the bitmap bit immediately. The real code only sets `FREE_KEY` and queues `pending_frees` (region.rs:144–147). Then `flush_pending_frees` (region.rs:152–160) drains them at checkpoint (lib.rs:318). The README calls this a "safe over-approximation". That claim is wrong for 2 reasons:
  - The model allows reuse before checkpoint, which FR-025 forbids, and it checks nothing about FR-025.
  - The real code reaches a state that the model never makes: bitmap set + `FREE_KEY` + no handle. P1 as written would reject that state.
- **Publish sentinel.** The model excludes the `key == FREE_KEY` branch of publish (lib.rs:600–607, FR-006), because `my_key = tid+1`.
- **Remove arguments.** The model's remove selects a valid victim. The real call takes an `offset`, and the model never runs its `OffsetNotFound` error paths (region.rs:127–138).
- **Allocator structure.** The model does not include: multiple slabs, the `SizeClassManager`, new-slab creation through the buddy allocator (region.rs:62–91), the return of an empty slab to the buddy allocator (region.rs:104–107), multiple regions, key sharding, the `dirty` flag, readers (`get_extents`), offsets and sizes, and checkpoint/recovery I/O.

### no-lost-extents
**Code that the model covers.**
- The `WriteHandle` RAII protocol (`interfaces/src/iextent_manager.rs`: struct at 95, `publish` at 132–139, `abort` at 141–146, `Drop` at 149–155). The README cites these correctly.
- The background write-through, `process_write_job` (disp:1390–1465):
  - `mt.peek` None → `release_read` with no reserve (1399–1404).
  - `reserve_extent` Err → no handle (1439–1445).
  - `write_buffer_to_ssd` Err → handle dropped → abort (1450–1456).
  - `publish` (1463).
- Shutdown drain: `BackgroundWriter::worker_loop` drains on shutdown (background.rs:126–137). The code reaches it through `IDispatcher::shutdown` (disp:2112, writer shutdown at 2123).

**Stale citations.** The `Client` "direct path" models `prepare_store`/`commit_store`/`cancel_store` and `pending_writes`. None of these exist in the current code (grep finds nothing). The dispatcher specification marks FR-020–FR-022 **REMOVED 2026-07-02** (l.297–299). So the direct-path branch and `ShutdownCleanup` model dead code. The README citations disp `1500–1663`, `350–431` and `808–819` are stale: `process_write_job` is at 1390, and shutdown is at 2112 with no `pending_writes`.

**Specification.** Extent-manager FR-005/006/007 (l.324–333) and FR-033 (l.493). Dispatcher FR-004 (l.281), FR-017 (l.294), FR-014 (l.291) and SC-006 (l.391).

**Properties.**
- P1 is `assert(extent_state[k] != RESERVED)` at quiescence (pml:317).
- P3 and P5 are the owner and state asserts in publish/abort (pml:92–93/105–106) and in ShutdownCleanup (pml:248–249).
- The model does not assert P2. The model never assigns the `ABORTED` mtype, because abort → FREE.
- P4 is only structural.

**Bounds.** `N_CLIENTS=2`, `N_EXTENTS=4`, `QUEUE_CAP=2`. There is 1 BgWriter and 1 ShutdownCleanup, and each client makes 1 request. Result: 4,254 states, depth 59, 0 errors, 0 not reached.

**Left out.**
- **Branches in the live path that the model does not include:**
  - the no-drives synthetic-offset branch (disp:1407–1411)
  - the `DmaBuffer::from_raw` failure (disp:1424–1434)
  - the discarded result of `publish` (`let _`, disp:1463)
  - the discarded result of `convert_to_storage` (disp:1464)
- **Missing components.** The model has no dispatch-map, so it does not include the read-ref release or `convert_to_storage`. It has no extent-manager internals: bitmap, deferred free, checkpoint. It has no `remove_extent` of published extents, so "lost" means only stuck-RESERVED, never orphaned-PUBLISHED.
- **Writer structure.** The real writer is a per-drive `ParallelBackgroundWriter` on an unbounded channel (background.rs:51/186–214). The model uses a single bounded queue.
- **dispatcher-p2p.** The model does not cover its own `process_write_job` and `reserve_extent` (`dispatcher-p2p/src/lib.rs:800`).

### populate-lookup-linearizability
**Code that the model covers.**
- The same populate chain as check-pin-eviction-race (disp:3143 → 3318 → 1176/1123 → 3489/3508/3552/3561, as 1 atomic commit + downgrade + `pop_ok`).
- `lookup_async` (disp:2994): `dm.lookup` (3011), the MemoryTier serve, then `release_read` (3035–3061), and BlockDevice `release_read` + `promote_and_serve` (3063–3066). The model shows the BlockDevice case as a plain pinned hit.
- `process_write_job` success and failure (disp:1390/1402–1464).
- The explicit `IDispatcher::remove` (disp:3096) through `dm.remove`, which rejects refs (dm:334–343).
- The model's `evict_one_clean`.

**The model's eviction is stale.** It contains a **drop branch** (`committed=false`, `departed=true` on a victim that is not persisted and not pinned). That is the behaviour before `1d55b9c2`. The current `evict_one_clean` (disp:1050–1106) has no `dm.remove`; it skips such victims. So the model checks P-LIN against the old evictor. P-LIN tolerates any `departed` key, so it is weaker than what the current code gives. The strict form is R-RESOLVE in check-pin-eviction-race.

**Stale citations.** All README line numbers are stale: 2860, 2907, 1015, 962, 3025, 3044, 3088, 3095–3102, 2711, 3676, 2753–2785, 920, 3730, 3810 and 2813. The current lines are above. Specifically, there is no `dm.remove` in `evict_one_clean`, and the `dm.remove` of `remove` is at disp:3119.

**Specification.** Dispatcher FR-003 (l.280), FR-006/007 (l.283–284), FR-013 (l.290), FR-024 (l.301, which the model's drop branch now contradicts), FR-060 (l.356), SC-001 (l.386) and SC-004 (l.389). Dispatch-map FR-003/004/010/011 (l.244/245/251/252).

**Properties.**
- P-LIN is `assert(!saw_pop_ok || saw_departed)` on a miss (pml:344).
- P-PIN is pml:351/353.
- P-REF is pml:137 (drop branch) and pml:286 (Remover).
- P-WJOB is pml:250–251.
- P-FIN is pml:425/427/429.

**Bounds.** `N_KEYS=2`, `POOL_CAP=1`, `N_LOOK=2`, `LOOKUPS=1`, `MAX_TRIES=4`. Processes: 2 Populators, 2 Lookers, 1 Remover (a single attempt), BgWriter and Evictor. Result: 7,354,797 states, depth 221, 0 errors, 0 not reached. This result matches the README.

**Left out.**
- **The model gives the wrong label to the real `populate` return value.** On `AllocationFailed`, the real `populate` returns `Ok(())` and does not cache (disp:3161–3174). The model labels this "populate returns Err" and keeps `pop_ok` false. So the model does not check the literal contract "after populate returns Ok, lookup never sees KeyNotFound".
- **The model drops some lookup outcomes.** `dm.lookup` `Timeout` → `KeyNotFound` (disp:3071) is a real way to miss a populated key that has not departed, and the model cannot make it. The model also drops: `MismatchSize` → `InvalidParameter`, the integrity-CRC failure, and `promote_and_serve` errors and their promotion write-refs.
- **The model collapses `remove`.** The real `remove` is not atomic: `dm.lookup` + `release_read` (3105–3115), then `dm.remove` (3119), then `mt.remove` (3124), then `remove_extent` (3130). The model does all of it in 1 atomic step, and it frees no extent.
- **Not in the model:** `batch_lookup`, `check`, `batch_populate`, `clear_memory_tier`, the SSD evictor, and the write-ref window between `create_memory_tier_entry` and `downgrade_reference`.
- **No key reuse.** Each key commits at most 1 time, so the model never runs the `gen` and `departed` reset logic.


### promotion-atomicity

**Code paths that the model covers.** This model shows the cold-promotion protocol as an *older* remove-and-recreate sequence. Each `LookupThread` does these steps:
- `dm.lookup`, which waits for `write_ref==0` and takes a read reference (DM:128-174);
- `release_read` before the promotion (`lookup_async` BlockDevice arm D:3063-3064; `batch_lookup` D:2428-2429);
- the keyed `mt.insert`, with the outcomes Ok, AlreadyExists or PoolFull (`evict_and_insert` D:1176-1225, insert at D:1188);
- a nondeterministic SSD read;
- `dm.remove` (DM:334-357);
- `create_memory_tier_entry`, which returns AlreadyExists if the key exists and otherwise sets `write_ref=1` (DM:393-439);
- `release_write` (DM:287-305).

The `Evictor` frees the MT slot and changes the entry to BlockDevice in 1 atomic step.

**The shipped code no longer uses this protocol.** The drive-backed `promote_and_serve` never calls `dm.remove`. It promotes in place with `promote_block_to_memory_tier` (D:902-904, DM:482-528), and the batch path does the same (D:2682, D:2491). `create_memory_tier_entry` plus `release_write` remain only in the no-drives branch (D:809-815). That branch has no `dm.remove` before it. So on a BlockDevice entry, `create_memory_tier_entry` hits AlreadyExists (DM:403-405), and the code tests the result only with `is_ok()`. The memory-tier evictor now uses `try_evict_to_block` (DM:548-580) and then a separate `mt.remove` (`evict_one_clean` D:1050-1107; `evict_for_space` D:1123-1167), not `convert_memory_tier_to_block`.

**All README citations are stale:**
- dm.lookup 143–183 → DM:128-174
- D:1349 is now `serve_concurrently_promoted`, not a `release_read`
- mt.insert 1489 → D:1188
- dm.remove D:1542 → no such call on the promote path
- create 1543–1553 → D:809-813
- release_write 1559 → D:815
- evict_for_space 483–542 → D:1123
- idispatch_map.rs:155 → trait method at :144
- DM remove 336–354 → 334-357
- DM create 377–389 → 393-439

**Specifications:**
- DSPEC FR-006 (:283), FR-013 (:290), FR-024 (:301), FR-028 (:305)
- DMSPEC FR-003 (:244), FR-004 (:245), FR-008/009 (:249-250), FR-011 (:252), FR-018 (:259)
- MTSPEC FR-009, insert rejects duplicates with AlreadyExists (:159)

**Properties and bounds.** There are no LTL formulas; the model checks everything with `assert`.
- P1: in-process `assert(mt_allocated[my_key])` (pml:204), plus `assert(dm_read_ref>0)` (pml:174) and `assert(dm_write_ref==1)` (pml:260).
- P3: final `dm_write_ref==0 && dm_read_ref==0` (pml:324-325).
- P2/P4: final `dm_state==MEMORY_TIER ⇔ mt_allocated` (pml:328-333).
- P5: final `mt_pool_used == #allocated` (pml:351).
- P2 as stated (at most 1 insert per lifecycle) has no direct assert. The model increments `promotion_success[]` (pml:265) but never asserts it. It records `my_gen`/`key_gen` but never compares them.

The bounds are `N_THREADS=3`, `N_KEYS=2` (threads 0 and 2 share key 0), `POOL_CAP=2`, at most 2 attempts per thread, 1 `Evictor`, and `init`. The run is `-DSAFETY ./pan -m200000`. The `liveness` target is `-a` with no never-claim.

**What the model leaves out:**
- Its failure cleanup does not match the code. The model undoes `mt.insert` when the SSD read fails or `create_memory_tier_entry` fails (pml:216-221, 247-251). The code has no `mt.remove` on those paths: `promote_and_serve` returns at D:883, ignores the create result at D:809-814, and the batch scatter error is `Err(e.clone())` at D:2710.
- No `ssd_offset`/persisted condition on eviction.
- The model's eviction is atomic, but the code does a DM transition and then a separate `mt.remove`.
- The model shows the DM condvar wait as a skip, with no `Timeout`/`NotExist`/`MismatchSize`.
- No staging-buffer fallback (D:765-786).
- No `serve_concurrently_promoted` recovery, no GPU DMA, no multi-drive or pipeline detail, no `max_eviction_attempts`. The PoolFull path does not evict inline.

### promotion-lookup-race

**Code paths that the model covers.** This model shows the in-place promotion protocol in `batch_lookup`:
- the classification `dm.lookup` pins on a hit (D:2353-2445, DM:128-174);
- the BlockDevice arm releases the pin before the promotion (D:2428-2429);
- the keyed `mt.insert` in `evict_and_insert` decides the winner, the AlreadyExists loser or PoolFull (D:1188-1217, called from D:2557-2581);
- the winner's fused SSD→GPU read (`gpu_dst` for 1 region, D:2562-2566), then `promote_block_to_memory_tier` (D:2682, DM:482-528);
- the warm MemoryTier arm (D:2398-2427);
- the `Evictor` abstracts `evict_for_space`→`evict_one_clean`→`try_evict_to_block` (D:1123, D:1050, DM:548).

**All README citations are stale:**
- in-place promote 3894/2486 → DM:482 and D:2491/2682; D:3894 is now `oldest_keys(SAMPLE)`
- recovery pass 2689 → D:2937-2945
- `serve_concurrently_promoted` 1181 → D:1349
- release_read 2205 → D:2429
- classify 2160–2214/3676 → D:2353-2445 / DM:128
- insert 2347–2383/1015 → D:2557-2581 / D:1188
- `gpu_dst` 2357 → D:2562
- warm arm 2175–2203 → D:2398-2427
- evict 962/920/3933 → D:1123 / D:1050 / DM:548
- NotExist 1216 → D:1384
- timeout 1204 → D:1358/1373

**Specifications:**
- DSPEC FR-051 (:328), FR-024 (:301), FR-028 (:305), FR-049 (:326)
- DMSPEC FR-025 (:266), FR-026 (:267)
- MTSPEC FR-009 (:159)

**Properties and bounds.** There is no LTL; the model checks everything with `assert`.
- A1: `win_count==1` on a win (pml:158) and `<=1` at rest (pml:307).
- A2: the warm observer sees `MT && mt_alloc && resident` before and after the serve window (pml:127, 130).
- A3: `read_refs==0` on eviction (pml:248).
- A4: `mt_pool_used==live` (pml:323).
- A5: final `read_refs==0` and `MT ⇒ alloc∧resident`, `BLOCK ⇒ !alloc` (pml:306-313).

The bounds are `N_KEYS=2`, `POOL_CAP=1`, `N_THREADS=3` (keyed `tid%2`), `MAX_ATTEMPTS=3`, 1 `Evictor`, and `init`. The run is `-DSAFETY -m200000`.

**Does the model show served-by? No.** The model does **not** show the served-by / tier label. It has no `tier[]` or `ServedBy` variable, and the recovery branch is a bare `skip`. So the model cannot check S002 FR-008/FR-009/FR-010 (:474-481).

In the code, the BlockDevice arm sets `tier[i]=Ssd` at D:2435. The recovery pass rewrites only `results[i]` (D:2941-2945). The code assigns `tier[]` only at D:2425, D:2435 and D:2875. So the mapping at D:2952-2956 labels a recovered loser `Ssd`. That reading of the code conflicts with S002 FR-010, which requires `DRAM`.

**What the model leaves out:**
- The model shows the loser's recovery as a full re-classify. On a BlockDevice re-read, this lets the loser try `mt.insert` again. The real `serve_concurrently_promoted` only looks up again, releases and sleeps (D:1356-1376). It also runs as a post-pass after remote lookup, not inline.
- The model releases warm pins immediately after the serve. The code keeps them in `warm_pins` until after the batched sync (D:2423, D:2754).
- No `write_ref`/condvar wait, lookup `Err` retries (D:2377-2386), `NotExist`, `MismatchSize` or 5 s timeout. The model uses exhausted attempts in place of the timeout.
- No SSD read failure. The scatter error path at D:2710 leaves the MT slot allocated, and the model never runs that path.
- No `promote_block_to_memory_tier` failure.
- No PoolFull→inline eviction, staging (D:2576-2578, D:2720-2737) or no-drives path (D:2480-2507).
- No remote lookup and no `ssd_offset` gating in the evictor. The model shows DM-demote plus `mt.remove` as atomic.
- No single-key `lookup_async`→`promote_and_serve` path. On that path, AlreadyExists propagates through `?` at D:3065 with no recovery.
- The model excludes the multi-region post-flip serve, as the README states.

### starvation-freedom

**Code paths that the model covers.** The model shows `ParallelBackgroundWriter`:
- `Client` mirrors `BackgroundWriter::enqueue`, which does `in_flight` += 1 and then `send` (BG:78-87);
- the routing follows `ParallelBackgroundWriter::enqueue` (BG:187-190);
- `Worker` mirrors `worker_loop`, which receives, processes and does `in_flight` −= 1 (BG:117-143);
- `init` mirrors the shutdown flag and drain (BG:108-115, BG:208-212).

The code starts the writer in `initialize` (D:2023-2035) and shuts it down at D:2123-2125. Jobs come from `copy_gpu_to_memory_completed` (D:3555-3566) and `schedule_write_through` (D:3855-3862).

**All README citations are stale:**
- enqueue 71–77 → BG:78-87
- worker 107–133 → BG:117-143
- in_flight 72/119 → BG:79-80 / 129, 135
- routing 174 → BG:188
- drain 121–128 → BG:132-137
- shutdown 96–105 → BG:108-115

**Specifications:**
- DSPEC FR-004 async write-through (:281)
- FR-014 shutdown completes in-flight work (:291)
- User Story acceptance 3, the writer drains on shutdown (:110)
- FR-017 silent drop (:294) applies only to the abstracted processing.

I found no DSPEC requirement that names per-drive writer threads, bounded queues or starvation freedom.

**Properties and bounds.** The asserts are:
- S1: `in_flight[d]>0` before decrement (pml:76) and `==0` at end (pml:187);
- S2: `total_processed==TOTAL_JOBS` (pml:194);
- L1 as a terminal-state assert `client_enqueued[c]==JOBS_PER_CLIENT` (pml:200).

L1/L2 liveness relies on `progress_worker`/`progress_client` labels (pml:82, 109) with `-DNP ./pan-live -l`. The LTL formulas `all_clients_finish` and `all_jobs_processed` (pml:215-216) are commented out, and Spin does not check them. The README says weak fairness, but neither Make target passes `-f`. The bounds are `N_CLIENTS=3`, `N_DRIVES=2`, `QUEUE_CAP=2`, `JOBS_PER_CLIENT=2`, so there are 6 jobs in total; `-m200000`.

**What the model leaves out:**
- The real channel is `crossbeam_channel::unbounded()` (BG:51), so `send` never blocks. The bounded-queue backpressure that the model studies does not exist in the code. The README admits this.
- The pml header claims pool-cap backpressure, but the model does not include it: there is no pool.
- The routing is `(client_id+job_seq)%N_DRIVES`, not `device_index % num_drives` with `device_index = drive_index(key, n)` (splitmix64, D:581-590).
- No `recv_timeout` 50 ms polling or `Disconnected` exit (BG:126, 140).
- No send-error rollback (BG:81-85) and no `flush` (BG:198-205).
- The model reduces `process_write_job` (D:1390-1462) to a nondeterministic skip, with no failure paths and no DM/MT effects.
- No store backpressure (`reserve_memory`, FR-060).
- The model assumes an orderly shutdown, after all clients finish.

### write-before-evict

**This model encodes an eviction design that the code no longer has.** It covers populate → write-through → eviction:
- `Client`: MT insert plus a DM entry, then `downgrade_reference`, then `enqueue`. The code is `populate` D:3144-3205, i.e. `reserve_memory` D:3318, `copy_gpu_to_memory_completed` D:3489-3568 (`create_memory_tier_entry` D:3508, `downgrade_reference` D:3552, enqueue D:3561).
- `BgWriter`: `process_write_job` D:1390-1462, with `mt.peek` None → `release_read` (D:1399-1404) and success → `convert_to_storage` (D:1461; DM:176-207 sets `ssd_offset` and decrements `read_ref`).
- `SsdEvictor`: `BackgroundEvictor::evictor_loop` BG:279-370 (`get_evictable_offset` BG:384-406, `mt.remove` + `dm.remove` BG:328-333).
- `evict_one`: a clean demote path plus a **blind-LRU** path. The blind path frees the slot unconditionally, then calls `convert_memory_tier_to_block`, else `dm.remove` (pml:58-153).

The current `evict_one_clean` (D:1050-1107) only calls `try_evict_to_block` and then `mt.remove`. It skips pinned candidates and candidates that are not persisted. It never frees blindly and never calls `dm.remove` (comment D:1084-1100; DSPEC FR-024 :301 and edge case :234). So the model's blind path and its `LOST`/`EVICTED_PENDING` states have no counterpart in the shipped code.

**All README citations are stale:**
- populate 1399–1498 → D:3144
- evict_one 296–348 / blind 336–347 → D:1050-1107, no blind path
- BgWriter BG:76–93 → BG:78-87 / 117-143
- process_job 350–431 → D:1390-1462
- SsdEvictor BG:158–239 → BG:279-370

The README also gives the parameters `POOL_CAP=3`, `QUEUE_CAP=4` and the properties P1–P3, but the .pml uses 2 / 1 and P1–P4.

**Specifications:**
- DSPEC FR-004 (:281), FR-005, FR-017 (:294), FR-024 (:301), FR-031/FR-032 (:308-309), FR-049 (:326)
- DMSPEC FR-005 (:246), FR-010 (:251), FR-011 (:252), FR-018 (:259), FR-026 (:267)

**Properties and bounds.** There is no LTL; the `ltl` that the README mentions is not in the .pml.
- P1 is structural, with no assert.
- P2: `has_ssd_offset` on a clean demote (pml:90) and on SSD-evict (pml:320); final `ON_SSD ⇒ has_ssd_offset` (pml:418).
- P3: final `LOST ⇒ !in_dispatch_map` (pml:420).
- P4: final `!in_dispatch_map ⇒ read_refs==0` (pml:422).
- Also `read_refs>0` before each writer release (pml:243-282), and the victim of the blind path exists (pml:123).

The bounds are `N_CLIENTS=2`, `N_KEYS=4` (2 disjoint keys per client), `POOL_CAP=2`, `QUEUE_CAP=1`, 1 `BgWriter`, 1 `SsdEvictor`, and `init`. The run is `-DSAFETY -m200000`.

**What the model leaves out:**
- The real write channel is unbounded (BG:51); the model's channel has a bound of 1.
- The `key_gen` generation guard has no counterpart in the code: `process_write_job` only tests `peek` for None.
- The model does not include the write-through failure paths (DmaBuffer wrap, `reserve_extent`, SSD write error → `release_read` without an offset, D:1418-1446) or the no-drives synthetic offset (D:1406-1411). The writer always succeeds while the slot is present.
- The model splits `convert_to_storage` into 2 atomic steps.
- No background `MemoryTierEvictor` (BG:471-576), store backpressure / drop-on-full (D:3159-3175), `write_ref`/condvar, or lookups or loads. Read references come only from the writer.
- No extent removal and no SSD-utilization thresholds.

## 2. promotion-lookup-race with the served-by label

**Does the original model show the label? No.** It has no `tier[]`/`ServedBy` variable, and the AlreadyExists (loser) branch is a bare `skip` (pml:194-200).

**What the code does** (acd24025, the code that green fixed today):
- The warm arm sets `tier[i] = Dram` (dispatcher lib.rs:2425).
- The BlockDevice arm sets `tier[i] = Ssd` *before* the promotion attempt (lib.rs:2435).
- The recovery pass (lib.rs:2937-2945) replaces only `results[i]`, with the warm serve from `serve_concurrently_promoted` (lib.rs:1349-1386). It never rewrites `tier[i]`.
- So the mapping at lib.rs:2952-2956 reports the loser as `Ssd`, but its data came from DRAM.

This violates specification 002 FR-010 ("A key served out of DRAM after waiting for a concurrent promotion MUST be attributed `DRAM`") and FR-009 (when the code overwrites a result, it must also overwrite the attribution).

**The smallest faithful change: 9 added lines, 1 changed** (`~/FV/spin_vs_loom/label.diff`).
- A per-thread `label` (L_NONE/L_SSD/L_DRAM). The model sets it exactly where the code sets `tier[i]`:
  - `L_DRAM` on the warm arm (lib.rs:2425);
  - `L_SSD` on the cold arm (lib.rs:2435).
- A `recovering` flag. The loser branch sets it, in place of the `skip`.
- While `recovering` is set, the classify sites do **not** assign the label again. This mirrors the code, where recovery is a separate pass that rewrites only `results[i]`.
- 2 assertions. They say that the label must agree with the source of the data:
  - **L1** (after a DRAM serve): `label == L_DRAM`;
  - **L2** (after the winner's own SSD read + flip): `label == L_SSD`.

We changed nothing else in the model: the protocol, the bounds and the existing asserts A1–A5 are the same as before.

**Spin results** (`make safety`: `spin -a`, `cc -O2 -DSAFETY`, `./pan -m200000`):

| copy | errors | states stored | transitions | depth | pan time | total memory | wall / peak RSS |
|---|---|---|---|---|---|---|---|
| original (Daniel's) | 0 | 39,626 | – | 107 | 0.02 s | 141.5 MB | 1.67 s / 146 MB |
| **+ label (same as the code)** | **1: `assertion violated (label==3)` = L1, at depth 68** | 8,158 | 13,664 | 114 | 0.01 s | 139.4 MB | 1.45 s / 143 MB |
| + label + fix (control: recovery changes the label to Dram) | 0 | 58,970 | 100,296 | 114 | 0.03 s | 142.8 MB | 1.40 s / 147 MB |

The control shows that L1/L2 are not too strong. When we fix the bug (FR-009/010), the model passes again. So the error comes from the bug, not from an artefact of the model.

**The trail** is at `~/FV/spin_vs_loom/promotion_lookup_race.pml.trail`. The full replay from `spin -t -p` is in `label_trail_replay.txt`. Its steps, in short: proc 4 = thread tid 2 (key 0), proc 2 = tid 0 (key 0), proc 3 = tid 1 (key 1).
1. **Steps 17–24 (proc 4):** It classifies key 0 as BLOCK, so `label = L_SSD`. It releases its read pin, wins `mt.insert` (`win_count = 1`, `mt_pool_used = 1`), and does the SSD read, so `resident[0] = 1`. It has not flipped yet.
2. **Steps 25–49 (proc 3, key 1):** It makes 3 tries, and each gives PoolFull (the pool cap is 1). It stops, so `threads_done = 1`. This is only pressure from the other key.
3. **Steps 50–52 (proc 2, key 0):** It classifies key 0 as BLOCK, so `label = L_SSD` (lib.rs:2435).
4. **Step 53 (proc 4):** `dm_loc[0] = MT` (promote_block_to_memory_tier); L2 `label == L_SSD` holds; the winner finishes.
5. **Steps 58–60 (proc 2):** It releases its pin. `mt.insert` finds `mt_alloc[0]`, so it gets **AlreadyExists** and becomes the loser; `recovering = 1` (lib.rs:2942).
6. **Steps 61–68 (proc 2):** The lookup again sees `MT` (serve_concurrently_promoted, lib.rs:1356). A2 holds, so the data is resident. It serves warm from DRAM. It is in recovery, so the model does not rewrite the label, and the label stays `L_SSD`.
7. **Step 69 (proc 2):** It releases its pin, then `assert(label == L_DRAM)` → **violated**. The code served the key out of DRAM and labelled it SSD.

That is exactly green's Loom finding. 2 `batch_lookup`s race on 1 SSD key. The loser waits for the winner's promotion and gets its data from DRAM, but the code still labels it SSD.

## 3. Spin vs Loom for these 8 properties

**What Spin gives that Loom does not give:**
- **Speed and scale.** An exhaustive search over a hand-written abstract model is fast and small: 1-11 s here, and the bug in 0.01 s. So the model can have 3+ actors, an evictor and a background writer together, plus bounded pools and retries, under a state-hash budget.
- **A trail that you can read step by step.**
- **Liveness and progress claims.** Non-progress cycles and LTL are available to `starvation-freedom`, but its LTL formulas are commented out.
- **System-level protocols across components and processes.** Spin can model dispatcher + dispatch-map + memory-tier + writer as 1 protocol, and it does not need compiled Rust. It also works before the code exists.

**What Loom gives that Spin does not give:**
- **It runs the real Rust**, or a disclosed mirror that is close to it: the actual `match` arms, return values and per-key arrays such as `tier[]`.
- **No silent abstraction.** In Loom, a field that the model forgot cannot be "abstracted away". That is exactly why Loom saw the label bug and Daniel's model could not see it: the model dropped `tier[]`.
- **It cannot become stale silently.** A Loom test breaks the build when the code changes. A Promela model becomes stale and nobody sees it, as step 1 shows: all 8 READMEs cite stale lines, and 4 models encode a protocol that the code no longer has.
- **Memory-model effects.** Loom explores real atomics and memory orderings (C11 relaxed/acquire/release).

**Cost of Loom.** It can explore only small thread counts and small bounded loops (the state explosion of real code). It has no liveness check other than deadlock. It needs the code to compile under `cfg(loom)` with loom primitives.

**For these 8 properties.** The protocol invariants (single winner, residency before flip, pin safety, pool accounting, no lost extents, write-before-evict) are a good fit for Spin. That is true only if the team keeps the models in sync with the code. Today 4 models are out of sync, and someone must run `tools-spin-sync`.

Loom is better for output-level properties, i.e. what the API *returns* (served-by attribution, the `populate` `Ok`-on-AllocationFailed, the `Timeout` → `KeyNotFound` outcome). The Promela models usually drop return values, and the bug was there.

**Recommendation.**
1. Add the label (as here) to `promotion-lookup-race` upstream.
2. Make `tools-spin-sync` require that the model includes every value that the API returns, or lists it as "left out".
3. Sync the 4 stale models again.

## Files
`~/FV/spin_vs_loom/`:
- `promotion_lookup_race.label.pml`, the label copy, and `label.diff`;
- `promotion_lookup_race.pml.trail` and `label_trail_replay.txt`;
- `label_safety.out`;
- `promotion_lookup_race.label-fixed.pml` with `fixed_safety.out`, the control;
- `base_safety.out`, the original.

Scratch copy: /tmp/j18.
