# Populate-Lookup Linearizability

**Scope:** Whole system (client → dispatcher → memory-tier / dispatch-map → background writer → memory-tier eviction → explicit remove)

**Synced to:** `origin/unstable` @ `acd24025` (J20). Every file:line below was found again by symbol
(`grep -n`) in that tree. `lib.rs` = `components/dispatcher/src/lib.rs`, `bg.rs` =
`components/dispatcher/src/background.rs`, `dm/lib.rs` = `components/dispatch-map/src/lib.rs`,
`mt/lib.rs` = `components/memory-tier/src/lib.rs`, `spec 001` =
`components/dispatcher/specs/001-dispatcher-cache-interface/spec.md`.

This Spin/Promela model checks the dispatcher's main visibility contract, in its **strict** form:

> After `populate(key)` returns `Ok` **and the block was cached**, a `lookup(key)` whose `dm.lookup`
> runs afterwards never returns `KeyNotFound`, **unless** an explicit `remove(key)` removed the
> dispatch-map entry in between.

The memory-tier evictor no longer has a drop branch. Commit `1d55b9c2` removed `dm.remove` from
`evict_one_clean` (lib.rs:1050-1108), and spec 001 FR-024 (l.301) now says unpersisted victims
"MUST NOT be dropped". So, unlike the previous model, P-LIN gives no escape for eviction.

`populate`'s **real** return value is modelled (`pop_ret[k]`). `populate` returns `Ok(())` in two
cases: after caching the block (lib.rs:3206), and on drop-on-full when the block was **not** cached
(lib.rs:3161-3174). The spec allows a later miss after a drop-on-full `Ok`:

- spec 001 FR-060 (l.358): "*if `reserve_memory` still returns `AllocationFailed` after
  backpressure, the store path MUST NOT propagate the error. It skips caching the block and reports
  success — `populate` returns `Ok(())` … leaving no dispatch-map entry, so a later lookup simply
  misses and recomputes.*"
- spec 001 FR-003 (l.280): "*`populate` best-effort **drops** the store — it returns `Ok(())` without
  caching the block (a later lookup misses and recomputes)*".
- spec 001 Edge Cases (l.227) and User Story 1 scenario 5 (l.39) say the same.

So the default build checks the spec contract. P-LIN applies only to `POP_OK_CACHED`, and P-DROP
asserts that a drop-on-full `Ok` leaves no entry. `-DLITERAL_OK` checks the literal reading ("any
`Ok` ⇒ no later miss") instead. Spin refutes that reading in 0.08 s, which is expected given FR-060.
Note that the interface doc comment (`interfaces/src/idispatcher.rs:658`) still says populate
"Returns `AllocationFailed` if the memory-tier pool is full". That is doc drift against FR-060 and
the code. It is not a model issue.

## Properties Verified

| ID      | Property | Type |
|---------|----------|------|
| P-LIN   | A lookup that returns `KeyNotFound` (dm `NotExist` → lib.rs:3029, or dm `Timeout` → lib.rs:3071) implies that, at the `dm.lookup` instant, populate had not yet returned Ok-cached, or a `remove()` had succeeded. **Strict**: no eviction-drop escape. | Safety |
| P-DROP  | A populate that returned Ok on drop-on-full (lib.rs:3174) left no dispatch-map entry (FR-060). | Safety |
| P-PIN   | While a MemoryTier hit holds its read-ref, the entry stays committed, MemoryTier, and resident. | Safety |
| P-REF   | `dm.remove` (remove()) and `try_evict_to_block` (evictor) succeed only when `read_ref == 0 && write_ref == 0`. | Safety |
| P-WJOB  | A queued write-through job finds its entry committed, this generation, MemoryTier, resident in the memory-tier, and pinned by the writer's read-ref. | Safety |
| P-CSLOT | **New.** When a cold `promote_and_serve` finishes its NVMe→DRAM read, the memory-tier slot it inserted at lib.rs:768 is still allocated to it, with the same allocation id. | Safety |
| P-RET   | **New.** Every returned label (populate, lookup, remove) matches the state at the code line that returns it. | Safety |
| P-FIN   | At quiescence: no leaked read/write refs. Every memory-tier slot belongs to a committed MemoryTier entry (**new: no orphan slot**). `pool_used` matches. Ok-cached and never removed ⇒ still committed. | Safety |

## System Abstraction

| Real component / operation | Promela process / construct |
|---|---|
| `populate(key)` lib.rs:3143-3207 | `Populator` (one per key), `do_populate` inline |
| `lookup` lib.rs:2197 → `lookup_async` lib.rs:2994-3073 (incl. cold `promote_and_serve` lib.rs:750-909) | `Looker` (`N_LOOK` concurrent) |
| Background writer `process_write_job` lib.rs:1390-1466 | `BgWriter` + `write_q` |
| `evict_one_clean` lib.rs:1050 (from `evict_for_space`/`evict_and_insert`) and `MemoryTierEvictor::evictor_loop` bg.rs:471 | `Evictor` proctype + `evict_one_clean` inline (called also from `Populator` and the cold path of `Looker`) |
| `remove(key)` lib.rs:3096-3136 (3 steps, not atomic) | `Remover` |
| SSD-capacity evictor `BackgroundEvictor::evictor_loop` bg.rs:279 | `SsdEvictor`, **only with `-DSSD_EVICTOR`** (off by default) |
| Dispatch-map entry exists / location / ssd_offset / refs | `committed[k]`, `location[k]`, `persisted[k]`, `read_refs[k]`, `write_ref[k]` |
| Memory-tier slot keyed `k` (+ allocation identity) | `in_pool[k]`, `slot_id[k]`, `pool_used`, `POOL_CAP` |
| `populate` return value | `pop_ret[k]` ∈ {`POP_OK_CACHED`, `POP_OK_DROPPED`, `POP_ERR_IO`} |
| `lookup` return value | Looker `ret` ∈ {`L_OK_MT`, `L_OK_SSD_PROMOTED`, `L_OK_SSD_STAGED`, `L_KNF_NOTEXIST`, `L_KNF_TIMEOUT`, `L_ERR_EXISTS`, `L_ERR_IO_PROMOTE`} |
| `remove` return value | `rm_ret` ∈ {`R_OK`, `R_KNF_TIMEOUT`, `R_KNF_ABSENT`, `R_KNF_PINNED`} |
| Successful `dm.remove` | `departed[k]` |

## Assumptions / Stubs

- **Populate visibility, two steps.** `create_memory_tier_entry` (lib.rs:3508, dm/lib.rs:393-439)
  makes the entry visible with `write_ref = 1`. `downgrade_reference` (lib.rs:3552, dm/lib.rs:307)
  then turns it into the writer's read-ref. These are two separate DM lock holds and are modelled as
  two atomic steps. While `write_ref` is held, `dm.lookup` waits (dm/lib.rs:131-135) or times out
  (dm/lib.rs:137-139 → lookup `KeyNotFound` lib.rs:3071; remove `KeyNotFound` lib.rs:3114). The
  2 s timeout is **over-approximated**: Timeout may happen whenever a write_ref is held. P-LIN still
  holds because populate sets `pop_ret` only after the downgrade.
- **`downgrade_reference` + `writer.enqueue` + `return Ok`** are one atomic step. Setting `pop_ret`
  there is no later than the real return, so it is stricter for P-LIN.
- **`dm.lookup` is atomic with its pin.** `wait_for` returns the guard still held (dm/state.rs:54-78),
  so it is modelled as a blocking guarded atomic.
- **Evictor is demote-only.** `try_evict_to_block` (dm/lib.rs:548-580) and then `mt.remove`
  (lib.rs:1068 / bg.rs:535) are two steps. A victim that is unpersisted, pinned, or BlockDevice is
  skipped.
- **Cold path.** `release_read` at lib.rs:3064 runs **before** `promote_and_serve` (lib.rs:3065), so
  the entry is unpinned during the cold read. Modelled exactly that way: mt.insert at 768, staging
  fallback at 770-780 (`L_OK_SSD_STAGED`), `AlreadyExists` at 1208-1217, NVMe read as a schedulable
  gap, then `promote_block_to_memory_tier` (903-904), which fails with IoError if the entry is gone.
- **Write-through can fail.** `BgWriter` nondeterministically persists (convert_to_storage 1464) or
  fails (release_read 1442/1454) the write.
- **Extents** (`remove_extent` lib.rs:3131, bg.rs:348; `reserve_extent` 1438) are not modelled.
- **Not modelled at all**: `batch_populate` (lib.rs:3211), `batch_lookup` (lib.rs:2228; it also
  releases the cold pin before cold prep, lib.rs:2428-2429), `promote_to_memory_tier` (lib.rs:3636),
  `clear_memory_tier` (lib.rs:3794; it force-`dm.remove`s unpersisted entries, lib.rs:3815-3819, a
  departure path through an explicit API), `pin`/`unpin`/`check`/`touch`, and the GPU staged
  primitives called directly by the shm-queue.
- **Spec/code ordering drift (not checked)**: spec 001 Edge Cases l.231 says remove frees "the
  memory-tier slot first, then the dispatch-map entry". The code does `dm.remove` (3119) before
  `mt.remove` (3124), on purpose (comment 3117-3118). The model follows the code.
- **Bounded channel.** `write_q` has `N_KEYS` slots. The real queue is unbounded
  (bg.rs:50-51), but the bound is never reached here.

## Returned values (skill 2d item 6)

Each value is either modelled (a variable set at the code line, plus an assert) or listed under
**Left out**.

| API | Value | Code line | Model | Assert |
|---|---|---|---|---|
| populate | `Ok(())` cached | lib.rs:3206 (via 3198, 3568) | `pop_ret=POP_OK_CACHED` | P-LIN, P-FIN |
| populate | `Ok(())` drop-on-full (not cached) | lib.rs:3161-3174 | `pop_ret=POP_OK_DROPPED` | P-DROP |
| populate | `Err(IoError)` D2H submit fail (slot freed 3476) | lib.rs:3474-3478 | `pop_ret=POP_ERR_IO` | P-RET, P-FIN |
| populate | `Err(IoError)` stream_synchronize fail (no slot release; also the null-stream sync inside copy_gpu_to_memory_async, 3482-3484) | lib.rs:3193-3194 | `pop_ret=POP_ERR_IO` | P-RET, P-FIN (finds candidate bug 1) |
| lookup | `Ok` MemoryTier serve | lib.rs:3036-3061 | `L_OK_MT` | P-PIN |
| lookup | `Ok` BlockDevice promoted | lib.rs:903, 3067 | `L_OK_SSD_PROMOTED` | P-RET, P-CSLOT |
| lookup | `Ok` BlockDevice staged (uncached) | lib.rs:770-780, 884-887 | `L_OK_SSD_STAGED` | P-RET |
| lookup | `Err(KeyNotFound)` from `NotExist` | lib.rs:3029 | `L_KNF_NOTEXIST` | P-LIN |
| lookup | `Err(KeyNotFound)` from dm `Timeout` | dm/lib.rs:137-139 → lib.rs:3071 | `L_KNF_TIMEOUT` | P-LIN |
| lookup | `Err(AlreadyExists)` cold insert race | lib.rs:1208-1217 → 788 | `L_ERR_EXISTS` | P-RET |
| lookup | `Err(IoError)` promote transition failed | lib.rs:903-904 | `L_ERR_IO_PROMOTE` | P-RET |
| remove | `Ok(())` | lib.rs:3135 | `R_OK` | P-RET, P-REF |
| remove | `Err(KeyNotFound)` from dm.lookup Err (Timeout) | lib.rs:3114 | `R_KNF_TIMEOUT` | P-RET |
| remove | `Err(KeyNotFound)` from dm.remove KeyNotFound | lib.rs:3119-3120 | `R_KNF_ABSENT` | P-RET |
| remove | `Err(KeyNotFound)` from dm.remove ActiveReferences | lib.rs:3119-3120, dm/lib.rs:341-343 | `R_KNF_PINNED` | P-RET |

### Left out (with file:line)

| API | Value | Code line | Why left out |
|---|---|---|---|
| populate | `Err(NotInitialized)` | lib.rs:3144 (ensure_initialized), 3182-3185, 3497-3505 | Lifecycle, not concurrency. |
| populate | `Err(InvalidParameter)` size 0 | lib.rs:3147-3151 | Input validation. |
| populate | `Err(InvalidParameter)` regions exceed slot | lib.rs:3441-3445 | Impossible for single-region populate (slot size = ipc size). |
| populate | `Err(AlreadyExists)` mt.insert duplicate | lib.rs:1208-1217 → 3176 | Model has one Populator per key, and no other path inserts k while k is uncommitted except a cold promote, which needs a committed BlockDevice entry. |
| populate | `Err(AlreadyExists)` / `Err(IoError)` from create_memory_tier_entry | lib.rs:3508-3518 | Same reason (no duplicate key). IoError needs eviction-policy or pool-id failure. |
| populate | `Err(KeyNotFound)` mt.get missing in copy/complete | lib.rs:3435, 3506 | Needs another thread to `mt.remove` a reserved, uncommitted slot. Nothing in the model does that. **Not checked**: `remove()` step 3 (lib.rs:3124) could do it for a re-populated key, but the model's Remover never runs against a re-populate. |
| populate | `Err(IoError)` integrity CRC / set_checksum |  lib.rs:3526-3549 | `integrity-check` feature is off by default (dispatcher/Cargo.toml:25,32). |
| populate | `Err(IoError)` downgrade_reference | lib.rs:3552-3553 | Unreachable (write_ref is held). |
| populate | `Err(e)` other reserve_memory errors (NotInitialized, InvalidParameter) | lib.rs:3176, 3341-3352 | Lifecycle/validation. |
| populate | `writer.enqueue` Err ignored | lib.rs:3561 | Sender lives as long as the writer (bg.rs:78-86). |
| lookup | `Err(NotInitialized)` | lib.rs:2999-3017 | Lifecycle. |
| lookup | `Err(InvalidParameter)` MismatchSize | lib.rs:3030-3035 | dispatch-map never returns `MismatchSize` (no match in dm/lib.rs). It exists only in the mock. |
| lookup | `Err(IoError)` integrity CRC mismatch | lib.rs:3038-3046 (verify_slot_crc 1233) | Feature off by default. |
| lookup | `Err(IoError)` serve_memory_tier_to_gpu GPU errors | lib.rs:1262-1345 → 3048-3061 | GPU failure, not visibility. Pin released either way (3056). |
| lookup | `Err(AllocationFailed)` no staging buffer | lib.rs:780-786 | Abstracted into `L_OK_SSD_STAGED`, i.e. a staging buffer is always assumed available. A real config without staging would return this error instead. |
| lookup | `Err(IoError)`/`Err(...)` from NVMe pipeline, channel checkout, DmaBuffer wrap, no-drives branch | lib.rs:794-817, 830, 849-861, 870-884 | Device errors. The no-drives branch (793-817) is a test mode. |
| lookup (sync) | `Err(IoError)` stream_synchronize | lib.rs:2204-2205 | Only reachable for non-null streams; GPU error. |
| remove | `Err(NotInitialized)` | lib.rs:3097-3102 | Lifecycle. |
| remove | `remove_extent` result ignored | lib.rs:3131 | Extents not modelled. |
| lookup/remove | dm.lookup `RefCountOverflow` | dm/lib.rs:146-149 | Needs 2^32 pins. |

**Returned values: modelled 15 / left out 20 / missing 0.**

## Correspondence to Source Code (re-derived at acd24025)

| Model location | Source | Lines |
|---|---|---|
| `do_populate` entry / size check left out | `populate` | lib.rs:3143, 3147 |
| `do_populate` phase 1 reserve loop | `populate` → `reserve_memory` → `evict_and_insert` → `evict_for_space` → `evict_one_clean`; `mt.insert` | lib.rs:3159 → 3318 (loop 3400-3418) → 1176 (insert 1188) → 1123 (1146) → 1050; mt/lib.rs:328 |
| `!reserved` → `POP_OK_DROPPED` | drop-on-full | lib.rs:3161-3174 |
| phase 2 D2H submit fail | `copy_gpu_to_memory_async` (`mt.remove` + IoError) | lib.rs:3192, 3474-3478 |
| phase 2 sync fail (**no slot release**) | `gpu.stream_synchronize(stream)?` | lib.rs:3193-3194 |
| `FIX_SYNC_LEAK` reference behaviour | `batch_populate` sync fail → `release_memory` | lib.rs:3295-3302 |
| phase 3a commit (`write_ref=1`) | `copy_gpu_to_memory_completed` → `dm.create_memory_tier_entry` | lib.rs:3198 → 3508; dm/lib.rs:393-439 |
| phase 3b downgrade + enqueue + Ok | `dm.downgrade_reference`, `writer.enqueue`, `Ok` | lib.rs:3552, 3561, 3568, 3206; dm/lib.rs:307-332 |
| `BgWriter` | `process_write_job`: `mt.peek`, failure `release_read`, `convert_to_storage` | lib.rs:1390, 1399, 1402/1432/1442/1454, 1464; dm/lib.rs:176-208 |
| `evict_one_clean` inline | `evict_one_clean` → `dm.try_evict_to_block` / `mt.remove` / skip | lib.rs:1050, 1066, 1068, 1073, 1076, 1106; dm/lib.rs:548-580 |
| `Evictor` | `MemoryTierEvictor::evictor_loop` | bg.rs:471, 523, 531, 535 |
| `Looker` dm.lookup (wait / Timeout / pin) | `lookup_async` → `dm.lookup` | lib.rs:3011; dm/lib.rs:128-174 (wait 131-135, Timeout 137-139, pin 146-149); dm/state.rs:54-78 |
| `Looker` NotExist / Timeout → KeyNotFound | `lookup_async` | lib.rs:3029, 3071 |
| `Looker` MemoryTier serve + release | `lookup_async` | lib.rs:3036, 3048, 3056, 3061 |
| `Looker` BlockDevice: release **before** promote | `lookup_async` | lib.rs:3063, 3064, 3065 |
| `Looker` cold insert / staging / AlreadyExists | `promote_and_serve` → `evict_and_insert`; staging; `mt.insert` AlreadyExists | lib.rs:750, 768, 770-787, 1208-1217 |
| `Looker` NVMe gap + promote transition | pipeline read; `promote_block_to_memory_tier` (IoError) | lib.rs:851-876, 884-887, 903-904; dm/lib.rs:482-528 |
| `FIX_COLD_PIN` reference behaviour | `batch_lookup` also releases before cold prep | lib.rs:2428-2429 (same pattern, not fixed by the switch) |
| `Remover` step 1 | `remove` → `dm.lookup` + `release_read` / Err → KeyNotFound | lib.rs:3096, 3105, 3107, 3111, 3114 |
| `Remover` step 2 | `dm.remove` (ActiveReferences / KeyNotFound → KeyNotFound) | lib.rs:3119-3120; dm/lib.rs:334-357 |
| `Remover` step 3 | `mt.remove(key)`, `remove_extent`, `Ok` | lib.rs:3124, 3131, 3135; mt/lib.rs:483-514 |
| `SsdEvictor` (opt-in) | `BackgroundEvictor::evictor_loop`: `oldest_keys`, `get_evictable_offset`, `mt.remove` **before** `dm.remove`, `remove_extent` | bg.rs:279, 314, 322 (384-404), 328, 331, 348 |

## Running

```bash
export PATH=$HOME/.local/bin:$PATH
make clean && make safety     # exhaustive safety (-DSAFETY)
make liveness                 # same model, non-SAFETY pan with -a (no accept labels → same search)
make clean && make safety MODEL=populate_lookup_linearizability_control.pml   # with the two fixes
spin -t -p -g populate_lookup_linearizability.pml                            # replay a trail
```

Build switches (pass to `spin -a`): `-DFIX_SYNC_LEAK`, `-DFIX_COLD_PIN` (the control turns both on),
`-DLITERAL_OK` (literal Ok contract), `-DNO_SLOT_CHECK` (diagnostic: drops P-CSLOT and the orphan-slot
P-FIN assert), `-DSSD_EVICTOR` (+ `-DFIX_SSD_EVICT_ORDER`).

## Tuning the Model

| Parameter | Default | Meaning |
|---|---|---|
| `N_KEYS` | 2 | keys / Populators |
| `POOL_CAP` | 1 | memory-tier slots (< N_KEYS forces eviction) |
| `N_LOOK` | 2 | concurrent Lookers |
| `LOOKUPS` | 1 | lookups per Looker |
| `MAX_TRIES` | 4 | reserve attempts before AllocationFailed (stands for max_eviction_attempts + backpressure budget) |

With the fixes (control), the default parameters take 36.75M states, about 33 s, and 4.3 GB. With
`-DSSD_EVICTOR -DFIX_SSD_EVICT_ORDER` and `N_LOOK=2` it is 231M states, 237 s, and 26 GB, which is
over the 24 GB budget. With `N_LOOK=1` it is 9.4M states, 6.3 s, and 1.0 GB. See RESULT.md.
