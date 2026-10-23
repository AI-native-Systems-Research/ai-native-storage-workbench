# J20 — promotion-atomicity — RESULT

Code: origin/unstable @ acd24025 (`/tmp/j20src`). Spin 6.5.2. Method: tools-spin-sync (J18 workbench version).
Status: **MODEL UPDATE REQUIRED -> UPDATED. Verification FAILS on the shipped code (3 candidate bugs). The control copy with the minimal fixes PASSES.**

## 1. What changed vs `~/FV/J20/orig/promotion-atomicity` (v2)

| # | v2 model | Code today (acd24025) | v3 model |
|---|----------|-----------------------|----------|
| 1 | Promotion = `dm.remove` + `create_memory_tier_entry` + `release_write` | In-place `dm.promote_block_to_memory_tier` (dispatcher lib.rs:903-904 -> dispatch-map lib.rs:482-528). `create_memory_tier_entry`/`release_write` remain only in the no-drives branch (lib.rs:809-815) | in-place BD->MT flip, results PB_OK / InvalidState / KeyNotFound; no-drives branch listed under Left out (L6) |
| 2 | Undoes `mt.insert` (mt_allocated=false) on SSD-read failure and on create failure | `return Err(e)` at lib.rs:883 and `?` at lib.rs:904. Neither calls `mt.remove` | no undo (FIX_UNDO=0). P4 now checked after each Err return |
| 3 | `mt.insert` failure ends the attempt silently (`goto try_next`) | `evict_and_insert` -> `Err(AlreadyExists)` (lib.rs:1208-1217) -> `Err(e) => return Err(e)` (789) -> `?` (3065) -> `?` (2198), returned to the `lookup` caller | `ret = R_ALREADY_EXISTS` + **P6** assert |
| 4 | PoolFull: give up | `evict_for_space`/`evict_one_clean` loop (1123-1168, 1050-1108), fragmentation retry (1190-1207), then staging fallback when has_drives (770-786, 893-896) | modelled, with MAX_ATTEMPTS=2 and staged Ok / IoError / AllocationFailed |
| 5 | Evictor: one atomic `mt.remove`+`convert_memory_tier_to_block` | `try_evict_to_block` (1066), then `mt.remove` (1068) as a separate step. Same in MemoryTierEvictor (background.rs:531, 535) | two steps; `try_evict_to_block` results modelled |
| 6 | no SSD evictor | BackgroundEvictor (background.rs:279-370): `get_evictable_offset` (dm.lookup+release_read), then `mt.remove` (328) BEFORE `dm.remove` (331) | `SsdEvictor()` added (`-DSSD_EVICTOR=0` removes it) |
| 7 | 2 retry attempts inside one thread | `lookup` makes one attempt and has no internal retry | each thread makes N_LOOKUPS independent `lookup` calls (1 by default) |
| 8 | P2 no direct assert; `promotion_success[]` never asserted; `key_gen` written but never read | — | P2 asserted at `mt.insert` (inserts_in_gen) and at the flip (`promotion_success==0`); P1 asserted at `mt.insert`, at the flip (slot owner) and at every third-party `mt.remove` (in_flight==0); `key_gen` removed |
| 9 | — | MemoryTier hit path, lib.rs:3036-3062 | modelled; **P7** slot live and owned |
| 10 | — | — | new **P8**: the holder's in-place promote must not fail (IoError at 904) |
| 11 | Correspondence table: every line stale (e.g. 1349, 1489, 1542-1559, 483-542) | — | re-derived by symbol: README table, .pml header, inline comments |
| 12 | Makefile | — | **unchanged** |

State-space hygiene: locals are reset after use (no semantic effect). N_LOOKUPS is 1 because 2 gives >74M states for the control (8 GB MEMLIM hit, incomplete). For reference, v2 itself passes at 232,317 states.

## 2. Returned values (summary row)

| Model | Status | Divergences | Verification | States | Unreached | Returned values |
|-------|--------|-------------|--------------|--------|-----------|-----------------|
| promotion-atomicity | UPDATED | items 1-7 above | **FAIL** (shipped: P6 first; also P1, P4, P7, P8 and the final checks); control PASS | 4,289 shipped (stops at first error) / 2,800,356 control | control: Evictor 0/57, SsdEvictor 0/56, LookupThread 18/320, init 0/67 | **16 / 12 / 0** (modelled / left out / missing) |

The full list is in README.md, sections "Returned values" (M1-M16) and "Left out" (L1-L12), each with file:line.

## 3. Results (`make clean && make safety`, `make liveness`; `/usr/bin/time -v`)

| Run | Errors | States stored | Transitions | Depth | pan elapsed | Total mem (MB) | Wall | Max RSS | Unreached |
|-----|--------|---------------|-------------|-------|-------------|----------------|------|---------|-----------|
| `make safety` (shipped) | **1** — `assert(ret != R_ALREADY_EXISTS)` (P6), depth 124 | 4,289 | 7,299 | 257 | 0.01 s | 139.170 | 2.19 s | 143,408 KB | not reported (stopped at error) |
| `make liveness` (shipped, `-a`) | **1** — same P6, depth 124 | 4,289 | 7,299 | 257 | 0 s | 139.072 | 2.71 s | 143,416 KB | not reported |
| control `make safety MODEL=promotion_atomicity_control.pml` | 0 | 2,800,356 | 4,362,139 | 659 | 1.18 s | 411.045 | 3.76 s | 421,944 KB | 18/320 LookupThread (see below), others 0 |
| control `make liveness MODEL=...control.pml` | 0 | 2,805,066 | 4,393,173 | 669 | 1.27 s | 411.045 | 3.69 s | 421,948 KB | same |

Wall time includes `spin -a` and `cc`.

Unreached in the control: 16 states are the second inline copy of `evict_one_clean` (the fragmentation retry at lib.rs:1201). It cannot be reached at POOL_CAP=2, because `can_allocate` == `used<cap` in the model. With `-DPOOL_CAP=1` the control passes at 9,056,764 states (5.11 s wall, 1,046,588 KB), and only 2 of 320 are unreached. Those 2 are the `R_IO_ERROR` arm after the promote, which P8 makes dead (it is reachable only when P8 fails), plus one skip.

Extra diagnostic runs (not Makefile; in /tmp, isolated with the hooks; all <1 s and <150 MB unless noted):
- shipped `-c0` (all errors, 18.08M states, 60 s cap, incomplete): P6, P1 (in_flight), P4, P7, P8 and final `!mt_allocated` all fire.
- FIX_RECOVER only: P8 fails first. FIX_RECOVER+FIX_UNDO: P8/P1 still fail (C3). FIX_RECOVER+FIX_PIN: P4 fails (C2). FIX_RECOVER+FIX_UNDO with SSD_EVICTOR=0: PASS (208,497 states). All three fixes: PASS.
- So each fix is needed: C1 needs FIX_RECOVER, C2 needs FIX_UNDO, C3 needs FIX_PIN.

## 4. Candidate bugs (counterexamples reachable in the real code)

Kept trails (each replays with `spin [flags] -t -p promotion_atomicity.pml`; text in `cex_*.replay.txt`):
`cex_shipped_P6_make.trail` (the make-safety trail, = `promotion_atomicity.pml.trail`), `cex_C1_already_exists.trail` (`-DSSD_EVICTOR=0 -DFIX_UNDO=1`), `cex_C2_slot_leak.trail` (`-DSSD_EVICTOR=0`), `cex_C3_ssd_evictor_frees_inflight_slot.trail` (no flags). C1-C3 are the shortest found (`pan -i`). Note: `make clean` deletes `*.trail`, so copy them aside before re-running make.

### C1 — single-key `lookup` returns `AlreadyExists` to the caller (P6). CANDIDATE BUG.
Trail `cex_C1_already_exists.trail` (only lookup threads A=tid0, B=tid2 on key 0, both cold):
1. A: `dm.lookup(key)` -> BlockDevice, read_ref=1 (dispatcher lib.rs:3011; dispatch-map lib.rs:147-157).
2. A: `let _ = dm.release_read(key)` (lib.rs:3064). The pin is dropped before promotion.
3. B: same two steps (3011, 3064). B also sees BlockDevice.
4. B: `promote_and_serve` -> `evict_and_insert` -> `mt.insert(key)` Ok (lib.rs:768, 1188; memory-tier lib.rs:389). B starts its SSD read (871).
5. A: `evict_and_insert` -> `mt.insert(key)` -> `MemoryTierError::AlreadyExists` (memory-tier lib.rs:357). `evict_and_insert` returns `Err(DispatcherError::AlreadyExists(k))` (lib.rs:1208-1217).
6. A: `Err(e) => return Err(e)` (lib.rs:789) -> `?` (lib.rs:3065) -> `?` (lib.rs:2198). `IDispatcher::lookup` returns `Err(AlreadyExists)`.

The key exists and B is about to serve it, yet A's caller gets an error. The spec says a BlockDevice lookup returns success (spec 001 US2 AS-2, `spec.md:61`). `contracts/errors.md:34` lists `AlreadyExists` only for `populate`/`prepare_store`. `batch_lookup` handles exactly this race (FR-051; lib.rs:2937-2945 -> `serve_concurrently_promoted` 1349-1388), but `lookup_async` has no such recovery. Callers: benches only in this tree (dispatcher_benchmark.rs:583/620 `.unwrap()`, dispatcher_hw_benchmark.rs:517/609). The vLLM connector uses `batch_lookup` per spec FR-039(3), so production impact is limited to direct `lookup`/`lookup_async` users.
The make-safety trail (`cex_shipped_P6_make.trail`) is a longer path to the same result. Keys are promoted and then demoted by `evict_one_clean` (try_evict_to_block 1066 + mt.remove 1068). Thread tid0's own `evict_for_space` (1157) then demotes key 0 between `try_evict_to_block` and `mt.remove`, and its `mt.insert` sees AlreadyExists.
**Minimal fix (FIX_RECOVER):** in `lookup_async`'s BlockDevice arm, on `Err(DispatcherError::AlreadyExists(_))` call `serve_concurrently_promoted(&dm,&gpu,key,from_ref(&ipc_handle),warm_raw)`, as batch_lookup does.

### C2 — failed promotion leaks the memory-tier slot (P4). CANDIDATE BUG.
Trail `cex_C2_slot_leak.trail` (one lookup thread):
1. `dm.lookup` -> BlockDevice (3011); `release_read` (3064).
2. `evict_and_insert` -> `mt.insert(key)` Ok, a slot is allocated (1188; memory-tier lib.rs:379-389).
3. The pipelined SSD read fails: `if let Some(Err(e)) = results...next() { return Err(e); }` (lib.rs:882-884). The same holds for `drive.channel_pool.checkout()?` (830) and the `NotInitialized` `?` (851-855). None of these calls `mt.remove(key)`.
4. `lookup` returns `Err(IoError)`. The slot stays in `mt`, but the dispatch-map entry is still BlockDevice.

Consequences:
- The slot can never be evicted. `evict_one_clean` -> `try_evict_to_block` gets `InvalidState("entry is not in memory-tier state")` (dispatch-map lib.rs:576), which is counted as "blocked_unpersisted" (1082). So capacity is lost permanently.
- Every later `lookup` of that key hits `mt.insert` -> `AlreadyExists` and fails as in C1, forever (no winner will ever flip it).

The same leak happens if `promote_block_to_memory_tier` fails at 904 (C3 makes that reachable). The batch path has the same pattern: `Err(e) => Err(e.clone())` at lib.rs:2710 and the promote-error arm at 2681-2691, with no `mt.remove`. That is outside this model; the sibling model should check it.
**Minimal fix (FIX_UNDO):** on any `Err` after `evict_and_insert` returned `Ok` (non-staged), call `let _ = mt.remove(key)` before returning.

### C3 — SSD-capacity evictor frees/drops a slot under an in-flight promotion (P1, then P8/P4). CANDIDATE BUG (needs SSD utilization >= 0.9).
Trail `cex_C3_ssd_evictor_frees_inflight_slot.trail`:
1. Lookup T (key 0): `dm.lookup` -> BlockDevice (3011); `release_read` (3064). Now read_ref=0.
2. BackgroundEvictor: `get_evictable_offset(dm,key)` -> `dm.lookup` BlockDevice -> `release_read` -> `Some(offset)` (background.rs:322-323, 388-391).
3. T: `evict_and_insert` -> `mt.insert(key)` Ok (1188). T owns the slot and starts the NVMe read into it (871).
4. BackgroundEvictor: `let _ = mt.remove(key)` (background.rs:328). It frees T's slot while T's DMA is still writing into it: **P1 violated** (use-after-free of the DRAM slot; another key can be allocated there).
5. BackgroundEvictor: `dm.remove(key)` succeeds, because read_ref=0 since T dropped its pin at 3064 (dispatch-map lib.rs:341-344). It then calls `em.remove_extent(offset)` (background.rs:348) while T is still reading that extent.
6. T: `dm.promote_block_to_memory_tier` -> `KeyNotFound` (dispatch-map lib.rs:496) -> `IoError("promote transition failed")` (904): **P8**.

Variants found by the `-c0` run: the evictor's `mt.remove` happens after T's flip, so the entry is MemoryTier with no slot and the next hit serves freed memory (**P7**). Or `dm.remove` fails (ActiveReferences) after `mt.remove` succeeded, which leaves an MT entry with a freed slot.
Comments at lib.rs:898-901 and the spec FR-024 rationale say pins protect in-flight loads. The single-key path drops its pin at 3064, and the SSD evictor frees DRAM before it checks refs in the dispatch-map. Compare `IDispatcher::remove`, which does `dm.remove` first "prevents new lookups from obtaining the memory-tier pointer while we free it" (lib.rs:3117-3125).
**Minimal fix (FIX_PIN, two parts):** (a) `lookup_async` keeps the read ref across `promote_and_serve` (move `release_read` 3064 after it; the in-place promote explicitly tolerates `read_ref>0`, lib.rs:898-901); (b) BackgroundEvictor calls `mt.remove(key)` only after `dm.remove(key)` succeeded (swap background.rs:328 and 331). With (a) alone, step 4 still fires, because `mt.remove` takes no dispatch-map pin.

### Control
`promotion_atomicity_control.pml` = `#define FIX_RECOVER 1, FIX_UNDO 1, FIX_PIN 1` + `#include "promotion_atomicity.pml"`. PASS on safety and liveness (table above), and PASS at POOL_CAP=1. No assert is weakened in the control. The hooks only change the modelled code. The one guard that depends on a hook is the `KeyNotFound` check, which accepts a KeyNotFound from the recovery path's 5 s timeout (lib.rs:1373), only under FIX_RECOVER. With `-DSTRICT_KNF=1` it fails, because the model lets the 5 s timeout fire at any time (a timing artifact, not a bug claim). Keep that in mind if FIX_RECOVER is adopted.

### Asserts I judged and kept
- P6 is a spec-level property (US2 AS-2, errors.md:34) and is not weakened. If the team decides `lookup` may return AlreadyExists, the spec and errors.md must say so.
- P1 at third-party `mt.remove` was first written as "slot owner is none/dm". That flagged a slot already leaked by C2 (not in flight) as a P1 failure. I narrowed it to `in_flight[k]==0` (no live promoter), which is what P1 says. The leak itself stays caught by P4.

## 5. Left out / not checked (honest list)

- `batch_lookup` (sibling model). The batch slot leak at lib.rs:2710 / 2681-2691 is noted but **not model-checked here**.
- No-drives branch (lib.rs:795-818). Timeouts and condvar (dm lookup Timeout). Integrity-check CRC. GPU DMA errors on the MT-hit path. `populate`/store paths that take `write_ref` (no writer is modelled, so write_ref is always 0). `IDispatcher::remove`, `clear_memory_tier`, `promote_to_memory_tier` prefetch (lib.rs:3636-3790, which also promotes in place and may share C2/C3, not checked).
- The extent manager (`remove_extent` and extent reuse in C3 step 5) is not modelled. The SSD-side corruption in C3 is inferred from code reading, not checked.
- N_LOOKUPS=1 (2 is intractable for the control under 8 GB). MAX_ATTEMPTS=2. LRU order abstracted.
- Liveness: `make liveness` runs `-a` without a never claim (acceptance cycles "not selected"), so it only re-checks safety plus invalid end states. No progress property is checked.
- C1-C3 are model counterexamples matched to code lines by reading. They were **not reproduced on hardware or in a Rust test**.
