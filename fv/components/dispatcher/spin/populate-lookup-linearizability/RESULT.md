# J20 — populate-lookup-linearizability: RESULT

Code: `origin/unstable` @ `acd24025`, from `/tmp/j20src`. Method: `~/FV/J20/tools-spin-sync.SKILL.md`, including the "Added in the
workbench" parts. Spin 6.5.2. Host: 64 cores, 503 GB.
`lib.rs` = `components/dispatcher/src/lib.rs`, `bg.rs` = `components/dispatcher/src/background.rs`,
`dm/lib.rs` = `components/dispatch-map/src/lib.rs`, `spec 001` = `components/dispatcher/specs/001-dispatcher-cache-interface/spec.md`.

**Status: MODEL UPDATE REQUIRED → updated. The model FAILS on today's code. I found 2 candidate bugs (plus 1 more in
an opt-in variant). Each one has a minimal fix, and the control copy passes with 0 errors. The strict P-LIN holds on today's code.
The "populate Ok then KeyNotFound" counterexample exists, and spec FR-060 explicitly allows it.**

## 1. What changed vs `~/FV/J20/orig/populate-lookup-linearizability`

| # | Divergence (skill 2d) | Old model | Today's code | New model |
|---|---|---|---|---|
| 1 | Removed path | `evict_one_clean` had a **drop** branch (`committed=false; departed=true` on an unpersisted, unpinned victim) | `evict_one_clean` lib.rs:1050-1108 has no `dm.remove`. It calls `try_evict_to_block` at 1066 and skips on `ActiveReferences` (1073) or any other Err (1076). Commit 1d55b9c2. Spec 001 FR-024 l.301: "it MUST NOT be dropped" | Drop branch deleted. Evictor is demote-only. P-LIN is **strict**: the only departure is `remove()` |
| 2 | Semantic | Evict scan+act was one atomic step | `try_evict_to_block` (DM lock) and `mt.remove(cand)` (lib.rs:1068 / bg.rs:535) are **two** steps | Two atomic steps |
| 3 | Semantic | `remove()` was atomic | lib.rs:3096-3136: `dm.lookup`+`release_read` 3105-3111 → `dm.remove` 3119 → `mt.remove(key)` 3124 → `remove_extent` 3131 | Three steps. Return value per step |
| 4 | Semantic | Commit + downgrade + Ok were one atomic step | `create_memory_tier_entry` 3508 (entry visible with `write_ref=1`, dm/lib.rs:393-439) and `downgrade_reference` 3552 are separate DM lock holds | Split. The window where `write_ref` is held is now visible to lookups (wait or **Timeout**) |
| 5 | Returned value | populate failure labelled "Err" | `AllocationFailed` → **`Ok(())`** without caching (lib.rs:3161-3174, FR-060) | `pop_ret ∈ {OK_CACHED, OK_DROPPED, ERR_IO}`. P-DROP added. `-DLITERAL_OK` checks the literal contract |
| 6 | Returned value | dm.lookup Timeout not modelled | `Err(Timeout)` dm/lib.rs:137-139 → `KeyNotFound` lib.rs:3071 (lookup), 3114 (remove) | `L_KNF_TIMEOUT` / `R_KNF_TIMEOUT` (over-approximated). Covered by P-LIN |
| 7 | New state | Cold hit = pin, serve, release | lib.rs:3063-3065: **`release_read` before `promote_and_serve`**. Then `mt.insert` (768), staging fallback (770-780), `AlreadyExists` (1208-1217), NVMe read, `promote_block_to_memory_tier` (903-904, IoError) | Modelled. `slot_id[]` added to track which allocation a slot is. P-CSLOT added |
| 8 | New state | D2H always succeeded | D2H submit failure frees the slot (3474-3478). Sync failure (3193-3194) does **not** free it | Both modelled |
| 9 | New property | P-FIN said nothing about memory-tier slots | — | P-FIN: no orphan slot, and `pool_used` is consistent. P-RET on every label |
| 10 | Line drift | Every README/pml citation | All re-derived by symbol. For example, `populate` was cited at 2860 and is now 3143; `evict_one_clean` 920 → 1050; `dm.remove` "3810" (that was the mock) → dm/lib.rs:334 | Correspondence table rebuilt |
| 11 | Opt-in (new) | — | SSD-capacity evictor bg.rs:279: `mt.remove` (328) **before** `dm.remove` (331) | `SsdEvictor` under `-DSSD_EVICTOR` (spec 001 FR-031). Off by default, so the default P-LIN stays strict as the task asked |

Makefile: unchanged. The control copy is run with `make … MODEL=populate_lookup_linearizability_control.pml`.

## 2. Returned values (summary row)

| Model | Status | Divergences | Verification | States | Unreached | Returned values (modelled / left out / missing) |
|---|---|---|---|---|---|---|
| populate-lookup-linearizability | UPDATED | 11 (above) | **FAIL** (candidate bug 1, P-FIN) | 1,393 (stops at first error) | n/a (stopped early) | **15 / 20 / 0** |
| …_control (2 fixes) | — | — | PASS | 41,944,255 | Populator 2/131, Looker 1/148 (see below) | 15 / 20 / 0 |

The full tables are in README.md, under "Returned values" and "Left out".

## 3. Results

Every run uses `-m200000`. Wall time and max RSS come from `/usr/bin/time -v` around `make`, so they include `spin -a` and `cc`.

| Run | Errors | States stored | Transitions | Depth | pan elapsed | Total mem (pan) | Wall | Max RSS | Unreached |
|---|---|---|---|---|---|---|---|---|---|
| orig model (reference, /tmp copy) | 0 | 7,354,797 | 16,026,106 | 221 | 5.25 s | 808 MB | 7.1 s | 0.83 GB | 0 |
| **`make clean && make safety`** | **1** (P-FIN orphan slot) | 1,393 | 2,320 | 255 (err at 211) | 0.00 s | 139 MB | 2.35 s | 0.14 GB | n/a (stopped) |
| **`make liveness`** | **1** (same assert) | 1,393 | 2,320 | 255 | 0.00 s | 139 MB | 1.64 s | 0.14 GB | n/a |
| control `make safety` | 0 | 41,944,255 | 79,858,317 | 307 | 43.5 s | 4,763 MB | 46.2 s | 4.88 GB | Populator 2 (`mt_free` else-skip, l.164), Looker 1 (`L_ERR_IO_PROMOTE`, l.542, unreachable once pinned) |
| control `make liveness` | 0 | 41,944,255 | 79,858,317 | 307 | 41.0 s | 4,763 MB | 44.0 s | 4.88 GB | same |
| variant `-DFIX_SYNC_LEAK` (bug 1 fixed only) | 1 (P-CSLOT) | 134,067 | 239,299 | 274 (err 221) | 0.11 s | 150 MB | 0.13 s | 0.16 GB | n/a |
| variant `-DNO_SLOT_CHECK` (today's code, slot asserts off) | **0** | 43,169,208 | 81,126,709 | 305 | 37.4 s | 4,883 MB | 37.7 s | 5.0 GB | Populator 1/122 |
| variant `-DLITERAL_OK` (+2 fixes) | 1 (P-LIN literal) | 131,478 | 234,979 | 257 (err 129) | 0.07 s | 150 MB | 0.08 s | 0.15 GB | n/a |
| variant `-DSSD_EVICTOR` (+2 fixes) | 1 (P-CSLOT) | 400,751 | 760,001 | 283 | 0.34 s | 174 MB | 0.36 s | 0.18 GB | n/a |
| `-DSSD_EVICTOR -DFIX_SSD_EVICT_ORDER` (+2 fixes), N_LOOK=2 | 0 | 231,443,160 | — | 315 | 237 s | — | ~4 min | **26.0 GB (over the 24 GB bound)** | not recorded |
| same, **N_LOOK shrunk to 1** | 0 | 10,693,340 | 20,308,084 | 278 | 7.21 s | 1,108 MB | 7.3 s | 1.14 GB | Looker 2/148, Populator 2/131 |

Notes:
- `make liveness`: the model has no `accept` labels and no never-claim. pan prints "no accept labels are defined, so option -a has no
  effect". So the liveness target re-checks the same safety properties (plus invalid end states) **without** `-DSAFETY`. **No liveness
  property is checked.** This is the same as the original model's Makefile.
- The 26 GB run went over the RSS bound. I am reporting it, and I report the shrunk N_LOOK=1 run beside it. This applies only to the
  opt-in SSD-evictor variant. The default model and the control are within bounds.
- Logs: `run_safety.log`, `run_liveness.log`, `run_control_*.log`, `run_variant_*.log`. Trails: `trails/`. The default-model trail is also at
  `populate_lookup_linearizability.pml.trail`.

## 4. Answer to the main question: populate `Ok(())` on drop-on-full, then a KeyNotFound lookup

**Yes, Spin finds it immediately** (`-DLITERAL_OK`, 0.07 s, `trails/literal_ok_spec_allowed.*`). Key 0's populate wins the single slot.
Key 1's populate finds every candidate pinned or unpersisted, gives up after the backpressure retries, and returns `Ok(())` at lib.rs:3174
with no dispatch-map entry. A lookup(1) then gets `NotExist` → `KeyNotFound` (lib.rs:3029).

**The spec allows this. It is not a bug.** Spec 001 FR-060 (l.358): "*if `reserve_memory` still returns `AllocationFailed` after
backpressure, the store path MUST NOT propagate the error. It skips caching the block and reports success — `populate` returns `Ok(())`;
… leaving no dispatch-map entry, so a later lookup simply misses and recomputes.*" FR-003 (l.280), Edge Cases (l.227) and User Story 1
scenario 5 (l.39) say the same thing. So the contract the spec actually gives is "Ok **and cached** ⇒ no later miss". The default model checks that
(P-LIN) and also checks P-DROP (an Ok on drop leaves no entry). Both hold. Two related observations:
(a) At the API level, `Ok` alone does not tell the caller whether the block was cached. Only the `TierEventStats::store_drops_on_full`
counter (FR-058) shows it. A caller that needs the literal FR-007/SC-001 reading cannot get it from the return value.
(b) `interfaces/src/idispatcher.rs:658` still documents "Returns `AllocationFailed` if the memory-tier pool is full". That is doc drift
against FR-060 and the code.

**Strict P-LIN on today's code:** it holds. `-DNO_SLOT_CHECK` (today's code, only the slot asserts disabled) gives 0 errors over 43.2M states,
and the control gives 0 errors over 41.9M states. This includes the dm `Timeout` → `KeyNotFound` path (lib.rs:3071), which is now modelled. It cannot
break P-LIN, because populate does not return until after `downgrade_reference` has cleared `write_ref`, so a lookup that sees populate's `Ok`
cannot time out on populate's write_ref. A caveat I did not check: the real Timeout is 2 s of **any** writer holding write_ref. The only writer
on a committed key here is populate itself, and `promote_and_serve` does not take write_ref, so the model's write_ref windows are complete.

## 5. Candidate bugs

### Candidate bug 1: `populate` leaks its memory-tier slot when `stream_synchronize` fails

- **Assert:** P-FIN `!in_pool[k] || (committed[k] && location[k]==MT)` (pml l.595, default build). Trail: `trails/bug1_sync_leak.trail`
  (= `populate_lookup_linearizability.pml.trail`). Replay: `spin -t -p -g populate_lookup_linearizability.pml`.
- **Steps (plain words):**
  1. Two lookups of key 0 run first. Each sees `NotExist` → `KeyNotFound` (lib.rs:3029). This is legal, because populate has not returned.
  2. populate(0) reserves the only slot (reserve_memory lib.rs:3159 → mt.insert 1188). D2H completes, `create_memory_tier_entry` runs (3508),
     then `downgrade_reference` (3552), enqueue (3561), and `Ok` (3206).
  3. The writer persists key 0 (`convert_to_storage` 1464).
  4. populate(1)'s `evict_for_space` demotes key 0 (`try_evict_to_block` 1066, `mt.remove` 1068) and reserves the slot for key 1.
  5. populate(1)'s `gpu.stream_synchronize(stream)` fails (lib.rs:3193-3194). The `?` returns `Err(IoError)`, and **nothing frees the slot
     reserved in step 4**. No dispatch-map entry is ever created for key 1.
  6. At quiescence, the memory tier holds a slot keyed 1 that no dispatch-map entry owns.
- **Consequences in real code:** the slot is never evictable. `evict_one_clean`'s `try_evict_to_block` returns `KeyNotFound`, which is counted as
  "blocked_unpersisted" (1076-1083) and skipped, and `MemoryTierEvictor` skips it too (bg.rs:531). So the DRAM is lost. Also, every later
  `populate(1)` hits `mt.insert` → `AlreadyExists` (1208-1217) → `Err(AlreadyExists)` (3176). That key can never be cached again in this process.
  The same leak also exists for the null-stream sync inside `copy_gpu_to_memory_async` (3482-3484).
- **Spec:** FR-056 (l.339): "`release_memory(key)` releases a reserved-but-not-completed slot". `batch_populate` does release it on the same
  failure (lib.rs:3295-3302, `release_memory` at 3297). `populate` does not. No spec line allows the leak. **Reachable** whenever a GPU
  stream sync fails. That is rare and usually fatal for the GPU, so severity is low.
- **Minimal fix (control `FIX_SYNC_LEAK`):** in `populate`, on `stream_synchronize` error, call `self.release_memory(key)` before returning
  `IoError`, exactly as `batch_populate` does.

### Candidate bug 2: a cold `lookup` reads NVMe into a memory-tier slot that has been freed (and leaks a slot if the promote fails)

- **Assert:** P-CSLOT `in_pool[k] && slot_id[k]==my_slot` (pml l.518). It shows up once bug 1 is fixed (`-DFIX_SYNC_LEAK`, 0.11 s). Trail
  `trails/bug2_cold_unpinned_slot.trail`. Replay: `spin -DFIX_SYNC_LEAK -t -p -g populate_lookup_linearizability.pml`.
- **Root cause:** `lookup_async` releases the dispatch-map read-ref **before** the cold promote (`dm.release_read(key)` lib.rs:3064, then
  `promote_and_serve` 3065). While the NVMe → DRAM read is in flight, the entry is unpinned, so `dm.remove` (dm/lib.rs:341-343 only refuses
  `read_ref>0 || write_ref>0`) succeeds. Two independent `mt.remove(key)` calls frees **by key** and **after a gap**: evict_one_clean
  (lib.rs:1068) and remove (lib.rs:3124). Either one can then free the slot that the cold promote just inserted.
- **Steps (plain words, from the trail):**
  1. populate(1) returns Ok-cached. The writer persists it. populate(0)'s eviction demotes key 1 and frees its slot. populate(0) commits and
     returns Ok. The writer persists key 0.
  2. The background evictor runs `try_evict_to_block(0)` (lib.rs:1066 / bg.rs:531). Key 0 is now BlockDevice, but its `mt.remove(0)` step
     (1068 / bg.rs:535) has **not run yet**.
  3. lookup(0): `dm.lookup` → `BlockDevice` with a pin (3011). Then `release_read` (3064), so it is **unpinned**.
  4. remove(0): `dm.lookup`+`release_read` (3105-3111). `dm.remove(0)` **succeeds** (3119), because nothing pins the entry. `mt.remove(0)` (3124) frees
     key 0's old slot. remove returns `Ok` (3135).
  5. lookup(0) continues in `promote_and_serve`: `evict_and_insert` → `mt.insert(0)` succeeds (768) and allocates a **new** slot keyed 0. The NVMe
     read into that slot starts (851-876).
  6. The evictor's delayed `mt.remove(0)` from step 2 now runs and frees **lookup(0)'s new slot**.
  7. The NVMe read and the H2D copy use a pointer into freed memory-tier DRAM. That memory can be handed to another key's `mt.insert`, which
     means silent data corruption. Then `promote_block_to_memory_tier` fails (`KeyNotFound`, because the entry was removed in step 4) →
     `Err(IoError)` (903-904).
- **Second failure on the same path:** with P-CSLOT disabled, Spin then finds that when `promote_block_to_memory_tier` fails (the entry was removed during the
  cold read), the slot inserted at 768 is **never freed**. That leaks it exactly as in bug 1, and later `populate(key)` returns `AlreadyExists`.
  The P-FIN orphan-slot assert catches this (0.08 s, scratch run).
- **Spec:** FR-049 (l.326) says the demote-then-free order "prevents a race where concurrent lookups obtain a freed memory-tier pointer".
  FR-024 (l.301) says "eviction NEVER frees a slot a pinned in-flight load still points at". The cold path is not pinned, so FR-024 does not
  literally cover it. But no spec line allows a load to DMA into a freed slot, and FR-009/FR-018 (l.286, 295) say remove proceeds
  immediately without waiting. I call it a **candidate bug, not spec-allowed**. Reachability needs three things together: an explicit `remove(key)` (or the
  SSD evictor, see 3), an in-flight cold lookup of the same key, and an evictor paused between its two steps. That is a narrow window, but nothing
  in the code prevents it.
- **Minimal fix (control `FIX_COLD_PIN`):** keep the read-ref across `promote_and_serve`. Move `dm.release_read(key)` from lib.rs:3064 to
  after the call at 3065, on both the Ok and Err paths. Then `dm.remove` returns `ActiveReferences` during the cold read (remove →
  `KeyNotFound`, 3120). `promote_block_to_memory_tier` already allows a pinned entry ("works when the entry is pinned (read_ref > 0)",
  lib.rs:898-901 comment). The demote evictor already skips pinned entries. With both fixes the control passes (0 errors, 41.9M states).
  `L_ERR_IO_PROMOTE` becomes unreachable, so the promote leak goes away too. `batch_lookup` releases the same way (lib.rs:2428-2429), but it is
  **not modelled or checked** here.

### Candidate 3 (opt-in `-DSSD_EVICTOR`): the SSD-capacity evictor frees the slot before `dm.remove`, without checking

- Even with both fixes, adding `BackgroundEvictor` (bg.rs:279) breaks P-CSLOT in 0.34 s (`trails/bug3_ssd_evictor_slot.*`; replay with
  the flags in `.flags`). For a key it sees as BlockDevice (bg.rs:322, 384-404), it calls `mt.remove(key)` at bg.rs:328
  **unconditionally and before** `dm.remove` (331). If `dm.remove` then fails (`ActiveReferences`, because the cold lookup is pinned), the slot
  is already gone. In the trail, the SSD evictor frees key 0's old slot, the pinned cold promote inserts a new one, and the demote evictor's delayed
  `mt.remove(0)` frees it. The same code also allows a more direct interleaving: the cold promote inserts first, then bg.rs:328 frees that slot directly.
- **Spec:** FR-031 / Edge Case l.236 say "No memory-tier operation is needed (the entry was already evicted from DRAM)". The code does one anyway,
  and does it first. That is code/spec drift, and it is unsafe.
- **Minimal fix (`FIX_SSD_EVICT_ORDER`):** call `dm.remove` first, and `mt.remove` only if it returned Ok. That passes with 0 errors at **N_LOOK=1**
  (10.7M states, 7.2 s, 1.1 GB). At N_LOOK=2 it also passed (231M states), but at **26 GB RSS, over the bound**. I am reporting both. This is outside the
  task's strict scope (the task named remove() as the only departure path), so I keep it opt-in and not in the control.

## 6. Left out / not checked

- The left-out returned values (20 rows, each with file:line) are in README "Left out". The main ones: integrity-check CRC errors (feature off by
  default, Cargo.toml:25,32), `MismatchSize` → `InvalidParameter` (lib.rs:3030-3035; the real dispatch-map never returns `MismatchSize`, only the
  mock does), GPU/NVMe device errors inside `serve_memory_tier_to_gpu` / `promote_and_serve`, and `AllocationFailed` when no staging buffer exists
  (lib.rs:780-786; the model always assumes staging).
- Not modelled: `batch_populate`, `batch_lookup` (it has the same release-before-cold pattern at 2428-2429), `promote_to_memory_tier`,
  `clear_memory_tier` (it force-`dm.remove`s unpersisted entries, lib.rs:3815-3819), pin/unpin/check/touch, extents (`remove_extent`,
  `reserve_extent`), and the shm-queue staged primitives.
- dm `Timeout` is over-approximated: it may fire whenever a write_ref is held, not only after 2 s.
- No liveness (LTL/accept) property exists, so `make liveness` is a second safety run.
- Spec l.231 says remove frees the slot before the dispatch-map entry. The code does the reverse (3119 then 3124). The model follows the code. This is
  drift in the spec text, not checked further.
- I did not change any assert to get a pass. The control differs from the model only by `#define FIX_SYNC_LEAK` and `#define FIX_COLD_PIN`
  (the `diff` is empty apart from the 14-line header).
