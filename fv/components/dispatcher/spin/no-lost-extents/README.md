# No Lost Extents

> **What it checks:** SSD space (extents) is never lost or freed twice, across store, write-through, remove and the evictors.
> **Result:** re-synced to the code: the model fails (errors: 1); the control copy passes.
> **The bugs:** root cause #9 (remove leaks the extent of a persisted entry) and #32 (key u64::MAX); #9 repaired by GLM-5.3, not merged yet.
> **Certus commit:** `acd24025`.
> **History:** Daniel's model, re-synced to the code in J20 (Claude, 2026-10-17). `before_resync/` has Daniel's version and the diff.

**Scope**: Whole system (Client → Dispatcher → BgWriter → ExtentManager, with DispatchMap and the evictors)

**Code**: `origin/unstable` @ `acd24025`. All file:line citations below were found again by symbol in that tree (J20).

Verifies that every extent reservation (`reserve_extent`) ends in either `publish()`
(data committed to SSD) or `abort()`/drop (reservation freed). It also verifies that a
published extent stays reachable from the dispatch-map until something frees it
through `remove_extent`. An extent left RESERVED, or PUBLISHED but no longer
referenced by any dispatch-map entry, leaks SSD space that the running system can
never reclaim.

## Properties Verified

| ID | Property | Type |
|----|----------|------|
| P1 | No extent is RESERVED after shutdown completes | Safety |
| P2 | (legacy, structural) ABORTED is never a resting state; PUBLISHED and ABORTED are mutually exclusive | Safety |
| P3 | `reserve_extent` takes only FREE slots (no double reserve) | Safety |
| P4 | An errored write path always aborts (drop semantics) | Safety |
| P5 | Only the WriteHandle holder publishes or aborts its reservation | Safety |
| P6 | **new** Every SSD offset given to or held by the dispatch-map (`convert_to_storage`, `remove_extent` callers, and at rest) is a PUBLISHED extent of the same key | Safety |
| P7 | **new** After shutdown, every PUBLISHED extent is referenced by its key's dispatch-map entry (no orphaned extent) | Safety |

## System Abstraction

| Real component | Promela process / object |
|----------------|--------------------------|
| `IDispatcher::populate`, cold load (promote), `IDispatcher::remove` | `Client(k)`, one key per client |
| Background writer `worker_loop` + `process_write_job` | `BgWriter()` |
| MT evictor demote, SSD evictor, extent-manager checkpoint timer | `Evictor()` |
| `IDispatcher::shutdown` ordering + final checkpoint | tail of `init` |
| Extent manager slots (bitmap + keys + pending_frees) | `extent_state[] / extent_owner[] / extent_key[]` (+ `PENDING_FREE`) |
| Dispatch-map entry (location, ssd_offset, read_ref) | `dm_loc[] / dm_off[] / dm_rref[]` |
| Write-job channel (crossbeam unbounded) | `chan write_queue[QUEUE_CAP]` |

Removed since `orig`: the `Client` direct path (prepare_store / commit_store /
cancel_store) and `ShutdownCleanup` (pending_writes.clear()). None of these symbols
exist in today's code. Dispatcher spec 001 marks FR-020..FR-022 `~~REMOVED~~ (superseded 2026-07-02)`
(`dispatcher/specs/001-dispatcher-cache-interface/spec.md:297-299`).

## Switches

| Macro | 0 (default = today's code) | 1 |
|-------|-----------------------------|---|
| `FIX_REMOVE` | `remove()` frees an extent only if `dm.lookup` said BlockDevice | minimal fix: `dm.remove` reports the entry's SSD offset (BlockDevice offset **or** MemoryTier `ssd_offset`) under the map lock, and `remove()` frees it |
| `FIX_FREE_KEY` | key `u64::MAX` goes through publish + convert_to_storage | minimal fix: write-through skips `convert_to_storage` for `FREE_KEY` (or populate rejects it) |
| `PUBLISH_ERR` | `publish()` always Ok (today's only impl) | `publish()` may return Err, as the interface type allows |

`no_lost_extents_control.pml` is the same model with `FIX_REMOVE=1 FIX_FREE_KEY=1`.

## Assumptions / Stubs

- `write_buffer_to_ssd`, `mt.peek`, `DmaBuffer::from_raw` are nondeterministic success/failure.
- Memory-tier contents are not modelled. Only the dispatch-map entry and its `read_ref` are. `write_ref` is folded in, because populate's `downgrade_reference` happens before the job is enqueued.
- One drive and one extent manager. `drive_index` routing is identical on the write and free paths (lib.rs:581-590; background.rs:344-349), so it is not modelled.
- Offsets are slot indices. The no-drives synthetic offset `key*4096` is modelled as `slot = k` and is not checked against the extent manager (P6 is guarded by `!no_drives`).
- Key `FREE_KEY_CLIENT` (= 2) plays `u64::MAX`.
- Candidate keys of the evictors are chosen nondeterministically, which over-approximates `oldest_keys`.

## Returned values

| API (call site) | Value | Status | Model |
|-----------------|-------|--------|-------|
| `reserve_extent` (dispatcher lib.rs:1439; impl extent-manager lib.rs:584-631) | `Ok(WriteHandle)` | modelled | `last_reserve = R_OK`, assert at BgWriter |
| | `Err(OutOfSpace)` (region.rs:69,75,82) | modelled | `last_reserve = R_OUT_OF_SPACE`, assert |
| | `Err(NotInitialized)` (lib.rs:218 `region_for_key`) | left out | see below |
| `WriteHandle::publish` (lib.rs:1463, result discarded) | `Ok(Extent)`, normal key (extent-manager lib.rs:609-614) | modelled | `last_publish = R_OK`, assert after convert |
| | `Ok(Extent)`, key `FREE_KEY`: slot freed (lib.rs:600-606) | modelled | `last_publish = R_OK_DISCARDED`; P6 assert in `convert_to_storage` |
| | `Err(ExtentManagerError)` (type, iextent_manager.rs:132-139) | modelled (`PUBLISH_ERR=1`) | `last_publish = R_ERR`, P6 + post-publish assert |
| `dm.convert_to_storage` (lib.rs:1410, 1464, discarded) | `Ok` (dispatch-map lib.rs:184-207) | modelled | `last_convert = R_OK`, assert |
| | `Err(KeyNotFound)` (l.181) | modelled | `R_KEY_NOT_FOUND`, assert (unreachable while job holds read ref) |
| | `Err(InvalidState)` BlockDevice (l.187-191) | modelled | `R_INVALID_STATE`, assert |
| `dm.release_read` (lib.rs:1402, 1432, 1442, 1454, discarded) | `Ok` (dispatch-map lib.rs:278-284) | modelled | `last_release = R_OK`, assert |
| | `Err(KeyNotFound)` (l.272) / `Err(RefCountUnderflow)` (l.274-276) | modelled | `R_KEY_NOT_FOUND` / `R_ERR`, assert |
| `remove_extent` (lib.rs:3131, background.rs:348, discarded) | `Ok` (region.rs:142-149) | modelled | `last_rm_ext = R_OK`, assert |
| | `Err(OffsetNotFound)` (region.rs:127,132,136,138) | modelled | `R_OFFSET_NOT_FOUND`, assert |
| `dm.remove` (lib.rs:3119; background.rs:331) | `Ok` / `Err(KeyNotFound)` / `Err(ActiveReferences)` (dispatch-map lib.rs:334-358) | modelled | branches in `Client` / `Evictor` (`goto done`, skip) |
| `dm.try_evict_to_block` (background.rs:531; lib.rs:1066) | `Ok` / `ActiveReferences` / `InvalidState` (dispatch-map lib.rs:548-577) | modelled | `Evictor` demote branch |
| `IDispatcher::remove` | `Ok(())` / `Err(KeyNotFound)` (lib.rs:3108-3120, 3138) | modelled | `Client` remove branch |
| `IDispatcher::populate` | `Ok(())` best-effort drop on AllocationFailed (lib.rs:3160-3174) | modelled | first `Client` branch |

### Left out

- `reserve_extent` → `Err(NotInitialized)` (extent-manager lib.rs:215-218). Not reachable after `initialize()`, and it takes the same `release_read` branch as OutOfSpace (dispatcher lib.rs:1441-1444).
- `remove_extent` → `Err(NotInitialized)` from `region_for_offset` (extent-manager lib.rs:223-230). Same reason.
- `DmaBuffer::from_raw` → `Err(SpdkEnvError::DmaAllocationFailed)` (interfaces/src/spdk_types.rs:300-308). Only its branch is modelled; the variant is not, because no extent is involved yet (dispatcher lib.rs:1431-1434).
- `write_buffer_to_ssd` → `Err(DispatcherError)` (dispatcher lib.rs:666-…). Only the branch is modelled (lib.rs:1451-1456); its variants never reach the extent.
- `populate` other errors (InvalidParameter l.3147-3151, NotInitialized, IoError from D2H l.3191-3193, `copy_gpu_to_memory_completed` KeyNotFound/AlreadyExists/IoError l.3506-3518). These all happen before an entry or a job exists, so no extent is created.
- `promote_block_to_memory_tier` errors (dispatch-map lib.rs:488-489, 496, 511-514). An error leaves the entry BlockDevice; only the Ok flip is modelled.
- `IDispatcher::shutdown` → per-drive `checkpoint()` Err is logged (dispatcher lib.rs:2131-2135). Assumed Ok: the model's final checkpoint always flushes.
- `clear_memory_tier` (lib.rs:3794-3825), `dm.remove` on an entry without an SSD copy. Covered by the `mt.peek None` branch and by `Client` remove of an MT entry; not a separate process.
- `format_on_init=false` recovery (`for_each_extent` → `recover_extent`, lib.rs:1914-1925). Out of scope: it would re-attach published orphans after restart, but only by key, and only on restart.

## Running

```bash
make clean && make safety     # assertions + invalid end-states
make liveness                 # pan -a (no accept labels: same checks without -DSAFETY)
# control (fixes on):
spin -a no_lost_extents_control.pml && cc -O2 -DSAFETY -o pan pan.c && ./pan -m200000
```

## Tuning the Model

Parameters: `N_CLIENTS=3` (keys), `N_EXTENTS=2`, `QUEUE_CAP=3`, `FREE_KEY_CLIENT=2`.

- `N_EXTENTS < N_CLIENTS` makes OutOfSpace reachable.
- `QUEUE_CAP = N_CLIENTS`, so a send never blocks (the real channel is unbounded).
- Each evictor step picks its key and acts inside one atomic block (try_evict_to_block holds the map lock; the SSD evictor's lookup and remove remain separate steps). This keeps the control run at about 9.2M states.

## Correspondence to Source Code

| Model location | Source file | Line range |
|----------------|-------------|------------|
| `BgWriter` / `process_write_job` | `components/dispatcher/src/lib.rs` | 1390–1465 |
| `BgWriter` mt.peek None branch | `components/dispatcher/src/lib.rs` | 1399–1405 |
| `BgWriter` no-drives synthetic offset | `components/dispatcher/src/lib.rs` | 1407–1412 |
| `BgWriter` DmaBuffer::from_raw Err | `components/dispatcher/src/lib.rs` (+ `interfaces/src/spdk_types.rs` 293–315) | 1422–1435 |
| `BgWriter` reserve_extent call / Err | `components/dispatcher/src/lib.rs` | 1439–1445 |
| `BgWriter` write_buffer_to_ssd Err → drop | `components/dispatcher/src/lib.rs` | 1451–1456 |
| `BgWriter` publish + convert_to_storage | `components/dispatcher/src/lib.rs` | 1463–1464 |
| `BgWriter` loop / shutdown drain | `components/dispatcher/src/background.rs` | 117–143 (drain 131–137), shutdown 108–115 |
| `reserve_extent` inline | `components/extent-manager/src/lib.rs`, `region.rs` | 584–631; alloc_extent 41–92 |
| `publish_extent` inline | `components/interfaces/src/iextent_manager.rs`; `components/extent-manager/src/lib.rs` | publish 132–139; publish_fn 597–616 (FREE_KEY 600–606) |
| `abort_extent` inline | `components/interfaces/src/iextent_manager.rs`; `extent-manager/src/lib.rs` | abort 141–147, Drop 149–155; abort_fn 618–621 |
| WriteHandle struct / new | `components/interfaces/src/iextent_manager.rs` | 95–119 |
| `remove_extent` inline | `components/extent-manager/src/lib.rs`, `region.rs` | 680–684; remove_extent_by_offset 121–150 |
| `checkpoint` inline | `components/extent-manager/src/lib.rs`, `region.rs` | 313–320; flush_pending_frees 152–160 |
| `release_read` inline | `components/dispatch-map/src/lib.rs` | 267–285 |
| `convert_to_storage` inline | `components/dispatch-map/src/lib.rs` | 176–208 |
| `Client` populate | `components/dispatcher/src/lib.rs` | 3143–3200; drop-on-full 3160–3174; copy_gpu_to_memory_completed 3489–3569 (create 3508, downgrade 3552, enqueue 3559–3566) |
| `Client` cold load promote | `components/dispatcher/src/lib.rs`; `components/dispatch-map/src/lib.rs` | 903; promote_block_to_memory_tier 482–523 |
| `Client` remove | `components/dispatcher/src/lib.rs` | 3096–3139 (lookup 3105–3116, dm.remove 3119–3120, remove_extent 3127–3133) |
| `Client` remove: dm.remove | `components/dispatch-map/src/lib.rs` | 334–358 |
| `Evictor` MT demote | `components/dispatcher/src/background.rs`; `lib.rs`; `dispatch-map/src/lib.rs` | 531–537; evict_one_clean 1066–1071; try_evict_to_block 548–577 |
| `Evictor` SSD evict | `components/dispatcher/src/background.rs` | 313–349; get_evictable_offset 384–404 |
| `init` shutdown order | `components/dispatcher/src/lib.rs` | 2112–2137 (evictors 2115–2121, writer 2123–2125, checkpoint 2128–2137) |
| FREE_KEY constant | `components/extent-manager/src/slab.rs` | 5 |
| Dispatch-map Location | `components/dispatch-map/src/entry.rs` | 9–19 |
