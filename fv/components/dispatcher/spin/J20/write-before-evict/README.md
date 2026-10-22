# Spin/Promela Formal Verification Models

## write_before_evict.pml

Models the dispatcher's populate → write-through → eviction lifecycle to verify
that memory-tier eviction never produces dangling references: no dispatch-map
entry resolves to SSD data that was never written, and no dispatch-map entry or
in-flight populate uses a DRAM slot that was freed.

**Synced to:** `origin/unstable` @ `acd24025` (J20). Every file:line below was
re-derived from that tree by symbol. Paths are relative to `components/`.

**Status at this sync:** MODEL UPDATE REQUIRED (done) → verification **FAILS**
with a candidate real bug (P5, SSD evictor frees a re-populated slot). A second
finding is P7 (FR-033 counter label). See `RESULT.md`. The control copy
`write_before_evict_control.pml` turns on both minimal code fixes and passes.

### Properties Verified

| ID | Property | Type | Status vs v3 |
| -- | -------- | ---- | ------------ |
| P1 | `ssd_offset` is set only by the writer, after it wrote **this** entry's DRAM slot | Safety | kept |
| P2 | BlockDevice state requires `ssd_offset` (no dangling SSD reference) | Safety | kept |
| P3 | No dispatch-map MemoryTier entry resolves to a freed DRAM slot | Safety | kept, re-stated. v3 wording "LOST → !in_dispatch_map" is **legacy**: the blind-LRU path that produced LOST is gone from the code; the intent (a DRAM slot gone without SSD backing must not stay resolvable) is checked directly |
| P4 | `dm.remove` / `dm.try_evict_to_block` never succeed while read/write refs are held; at quiescence every write-through pin is released | Safety | kept |
| P5 | A DRAM slot is never freed while in use: while the dispatch-map resolves to it, while a populate is copying into it, or while the writer is writing it to SSD | Safety | **new** (FR-024: "the dispatch-map transition happens BEFORE the DRAM slot is freed") |
| P6 | `populate` returns only specified outcomes: `Ok` (cached), `Ok` (dropped on full), `AlreadyExists`; `downgrade_reference` never fails | Safety | **new** (spec 001 FR-003, FR-060, US1 AS3) |
| P7 | `evictions_blocked_unpersisted` is recorded only when `try_evict_to_block` refused for want of an `ssd_offset` | Safety | **new** (spec 002 FR-033) |

Legacy (kept as notes, no longer exercised): v3 `EVICTED_PENDING` cleanup and
`LOST` state. Source no longer exercises these paths: `evict_one_clean` never
blind-frees and never calls `dm.remove` (dispatcher/src/lib.rs:1050-1108; spec
001 FR-024 l.301, edge case l.234).

### System Abstraction

| Real component | Promela process / object |
| -------------- | ------------------------ |
| `populate()` caller thread | `Client(id)` (2 clients, 2 populates each: keys {0,1} and {2,0}) |
| `dispatcher-bg-writer-N` thread (`process_write_job`) | `BgWriter()` |
| `dispatcher-ssd-evictor` thread (`BackgroundEvictor::evictor_loop`) | `SsdEvictor()` |
| `crossbeam_channel::unbounded()` | `chan write_queue[QUEUE_CAP]`, QUEUE_CAP = total populates, so never full |
| Memory-tier pool | `mt_slot[]` (slot generation per key) + `pool_used` |
| DRAM slot identity (`*mut u8` from `mt.insert`) | `key_gen[]` → generation stored in `mt_slot[]` / `dm_ptr[]` |
| Dispatch-map entry | `dm_loc[]` (NONE/MT/BD), `dm_ptr[]`, `dm_ssd[]`, `dm_rref[]`, `dm_wref[]` |
| populate's D2H target (ghost) | `filling[]` |

`key_gen` changed meaning: in v3 it was a writer-side generation guard. The code
has no such guard (`mt.peek` is keyed, dispatcher/src/lib.rs:1399), so v4 uses it
only for slot identity.

### Running

```bash
export PATH=$HOME/.local/bin:$PATH     # Spin 6.5.2
make clean && make safety              # ./pan -m100000, -DSAFETY
make liveness                          # ./pan-live -a -m100000
# control copy (both fixes on):
mkdir -p control && cp Makefile write_before_evict_control.pml control/
(cd control && make MODEL=write_before_evict_control.pml safety)
# isolate P7 only:
spin -DFIX_SSD_EVICTOR -a write_before_evict.pml && cc -O2 -DSAFETY -o pan pan.c && ./pan -m100000
```

There are no LTL claims; `liveness` is the non-SAFETY build (`-a` with no
never claim → invalid end states + assertions, acceptance cycles "not
selected").

### Tuning the Model

- `N_CLIENTS=2`, `N_KEYS=3`, `POOL_CAP=2`, `QUEUE_CAP=4`, `MAX_ATTEMPTS=2`, `BP_RETRIES=1`
  (v3 README said POOL_CAP=3/QUEUE_CAP=4/P1-P3 while the v3 .pml had 2/1/P1-P4;
  both are replaced by the values above.)
- `POOL_CAP < N_KEYS` forces eviction. Key 0 is populated twice, so re-populate
  after demotion and the `AlreadyExists` paths are reached.
- `MAX_ATTEMPTS` stands for `max_eviction_attempts` (default 2048) and
  `BP_RETRIES` for the `store_backpressure_ms` budget (default 5000 ms); both
  are bounded, so 2 and 1 cover "retry, then give up".
- Control: 8.6M states, ~1 GB, ~5 s (< 10M target).

### Correspondence to Source Code

All at `acd24025`. `lib.rs` = `dispatcher/src/lib.rs`, `bg.rs` = `dispatcher/src/background.rs`.

| Model location | Source | Lines |
| -------------- | ------ | ----- |
| `Client` (whole populate) | `lib.rs` `populate` | 3143–3207 |
| Phase 1 reserve | `lib.rs` `reserve_memory` (backpressure loop 3400–3418) | 3318–3420 |
| Phase 1 drop-on-full `R_OK_DROPPED` | `lib.rs` `populate` `Err(AllocationFailed) => … return Ok(())` | 3161–3174 |
| `evict_and_insert` / `mt.insert` / PoolFull / AlreadyExists | `lib.rs` `evict_and_insert` | 1176–1224 (insert 1188, PoolFull 1190–1206, AlreadyExists 1208–1217) |
| `mt.insert` semantics | `memory-tier/src/lib.rs` `insert` | 328–390 (AlreadyExists 356–358, PoolFull 362–364) |
| evict_for_space loop, `R_ALLOC_FAILED` | `lib.rs` `evict_for_space` | 1123–1168 (attempt bound 1136–1152, scan-exhausted 1157–1163) |
| `evict_one_clean()` | `lib.rs` `evict_one_clean` | 1050–1108 (oldest_keys 1056, try_evict 1066, demote+mt.remove 1067–1071, pin 1073–1075, Err(_) 1076–1083, exhausted 1106–1107) |
| `try_evict_to_block` atomic block | `dispatch-map/src/lib.rs` `try_evict_to_block` | 548–580 |
| `mt_remove()` | `memory-tier/src/lib.rs` `remove` | 483–514 |
| Phase 2 `mt.get` + D2H | `lib.rs` `copy_gpu_to_memory_async` (mt.get 3435), `populate` 3192–3194 | 3422–3487 |
| Phase 3 `mt.get` | `lib.rs` `copy_gpu_to_memory_completed` | 3506 |
| `create_memory_tier_entry` + AlreadyExists → `mt.remove` | `lib.rs` 3508–3518; `dispatch-map/src/lib.rs` 393–439 | |
| `downgrade_reference` | `lib.rs` 3552–3553; `dispatch-map/src/lib.rs` 307–332 | |
| `write_queue ! my_key` | `lib.rs` 3559–3566; `bg.rs` `BackgroundWriter::enqueue` 78–85, unbounded channel 51–52 | |
| `BgWriter` loop | `bg.rs` `worker_loop` | 117–143 |
| `BgWriter` body | `lib.rs` `process_write_job` | 1390–1465 |
| peek None → release_read | `lib.rs` | 1399–1405 |
| reserve_extent Err / write Err → release_read | `lib.rs` | 1439–1444, 1451–1456 |
| publish + convert_to_storage | `lib.rs` 1463–1464; `dispatch-map/src/lib.rs` `convert_to_storage` 176–208 | |
| `SsdEvictor` | `bg.rs` `BackgroundEvictor::evictor_loop` | 279–370 |
| candidate + BlockDevice check | `bg.rs` oldest_keys 314, `get_evictable_offset` 322, 384–404 | |
| SSD evictor `mt.remove` (**finding 1**) | `bg.rs` | 327–328 |
| SSD evictor `dm.remove` | `bg.rs` 331–333; `dispatch-map/src/lib.rs` `remove` 334–357 | |
| remove_extent | `bg.rs` | 345–349 |
| background `MemoryTierEvictor` | `bg.rs` `MemoryTierEvictor::evictor_loop` 471–576 (try_evict 531, mt.remove 535) | not a separate process: same `try_evict_to_block` → `mt.remove` order as `evict_one_clean`, which every `Client` already runs; disabled by default (`memory_tier_eviction_threshold: 0.0`, `interfaces/src/idispatcher.rs:120`) |

### Returned values

Every value the modelled API returns on the modelled paths. "Modelled" = a
variable set at the same code line plus an assert on it.

| API / call site | Value | Code | Treatment |
| --------------- | ----- | ---- | --------- |
| `populate` | `Ok(())` cached | lib.rs:3206 | modelled `ret=R_OK_CACHED`, P6 assert |
| `populate` | `Ok(())` dropped-on-full | lib.rs:3161–3174 | modelled `ret=R_OK_DROPPED`, P6 |
| `populate` | `Err(AlreadyExists)` via mt.insert | lib.rs:1208–1217 → 3417 → 3176 | modelled `ret=R_ALREADY_EXISTS`, P6 |
| `populate` | `Err(AlreadyExists)` via create_memory_tier_entry | lib.rs:3510–3513 | modelled `ret=R_ALREADY_EXISTS`, P6 + P5 on the `mt.remove` |
| `populate` | `Err(KeyNotFound)` from `mt.get` | lib.rs:3435, 3506 | modelled `ret=R_KEY_NOT_FOUND`, P6 forbids it (reached only via finding 1) |
| `reserve_memory`/`evict_for_space` | `Err(AllocationFailed)` (3 sites) | lib.rs:1149–1151, 1160–1163, 1193–1196, 1203–1206 | modelled `ret=R_ALLOC_FAILED`; P6 asserts it never escapes populate |
| `evict_one_clean` | `true` / `false` | lib.rs:1071 / 1107 | modelled `freed` |
| `try_evict_to_block` | `Ok`, `ActiveReferences`, `InvalidState`(no offset), `InvalidState`(not MT), `KeyNotFound` | dispatch-map lib.rs:550–579 | modelled `tev`, P2/P4 on Ok, P7 on the counter label |
| counter label `evictions_blocked_by_pin` / `_unpersisted` | | lib.rs:1074 / 1082 | modelled `counted_unpersisted`, P7 |
| `mt.insert` | `Ok(ptr)`, `AlreadyExists`, `PoolFull` | memory-tier lib.rs:356–389 | modelled (slot generation, ret, attempts) |
| `mt.get` / `mt.peek` | `Some` / `None` | memory-tier lib.rs:392–433 | modelled (`ptr`, `wslot`); P3 assert on peek None |
| `mt.remove` | `Ok` / `KeyNotFound` | memory-tier lib.rs:483–514 | modelled; P5 asserts at every call; `KeyNotFound` ignored by all callers (`let _`) |
| `create_memory_tier_entry` | `Ok` / `AlreadyExists` | dispatch-map lib.rs:393–439 | modelled, P3 assert on Ok |
| `downgrade_reference` | `Ok` | dispatch-map lib.rs:307–332 | modelled, P6 assert it cannot fail |
| `convert_to_storage` (writer) | `Ok` | dispatch-map lib.rs:176–208 | modelled, P4 assert `dm_loc==DM_MT` (result ignored at lib.rs:1464) |
| `release_read` (writer, 4 sites) | `Ok` | dispatch-map lib.rs:267–285 | modelled, assert entry exists and ref > 0 (results ignored) |
| `dm.remove` (SSD evictor) | `Ok` / `ActiveReferences` / `KeyNotFound` | dispatch-map lib.rs:334–357 | modelled, P4 + P2 |
| `get_evictable_offset` | `Some(offset)` / `None` | bg.rs:384–404 | modelled `is_bd` |

**Summary: 18 rows modelled / 9 left out / 0 missing.**

### Left out (with reason)

| Value / path | Code | Why left out |
| ------------ | ---- | ------------ |
| `populate` `Err(NotInitialized)` | lib.rs:3144, 3182–3185 | setup error before any state change |
| `populate` `Err(InvalidParameter)` size 0 | lib.rs:3147–3151 | precondition, no state change |
| `copy_gpu_to_memory_async` `Err(InvalidParameter)` regions > slot | lib.rs:3442–3446 | not reachable for populate (one region of the reserved size); leaves the slot reserved (no `mt.remove`) — not modelled |
| GPU errors `Err(IoError)` (memcpy_d2h_async, stream_synchronize) | lib.rs:3469–3478, 3482–3484, 3193–3194 | hardware fault; the async-copy failure does `mt.remove` (3476) like the AlreadyExists path; stream-sync failures leave the slot reserved — not modelled |
| `create_memory_tier_entry` other errors → `IoError` | lib.rs:3514–3517 | `InvalidSize`/`NotInitialized` impossible here (size>0, policy bound) |
| integrity-check `set_checksum` / sync errors | lib.rs:3526–3550 | `#[cfg(feature = "integrity-check")]`, off by default |
| `DmaBuffer::from_raw` `Err` → release_read | lib.rs:1421–1435 | cannot fail: ptr from `mt.peek` non-null, `aligned_bytes > 0` (interfaces/src/spdk_types.rs:299–308) |
| no-drives synthetic offset path | lib.rs:1407–1411 | memory-tier-only test mode (spec 001 l.111); no SSD evictor is started without extent managers (lib.rs:2053) |
| `enqueue` `Err(job)` (disconnected) | bg.rs:78–85, ignored at lib.rs:3561 | only after shutdown; would leave the read pin held |
| `WriteHandle::publish` result | lib.rs:1463, interfaces/src/iextent_manager.rs:132–139 | always `Ok` (extent-manager/src/lib.rs:597–614) |
| `lookup`/`batch_lookup` cold promotion (`promote_block_to_memory_tier`) | lib.rs:2428–2443, 2558, 2682 | not part of the write-before-evict lifecycle; **it also does `mt.insert` on a BlockDevice key, so finding 1 very likely applies there too — not checked** |
| `MemoryTierEvictor` thread | bg.rs:471–576 | same ordering as `evict_one_clean` (see Correspondence); default off |
