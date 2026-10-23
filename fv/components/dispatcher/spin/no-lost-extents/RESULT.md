# J20 — no-lost-extents — RESULT

Code: `origin/unstable` @ `acd24025`, read-only export `/tmp/j20src/components/...`. Every line number below refers to that tree.
Tool: Spin 6.5.2 (`~/.local/bin/spin`), gcc `-O2`. Method: `~/FV/J20/tools-spin-sync.SKILL.md`, including the "Added in the workbench" parts.

**Sync status: MODEL UPDATE REQUIRED → updated. Safety: FAIL. Liveness: FAIL (same assertion). Candidate bug: YES (one, plus one minor edge case and one latent hazard).**

## 1. What changed vs `~/FV/J20/orig/no-lost-extents`

| # | Item | Why (code lines) |
|---|------|------------------|
| 1 | **Removed** the `Client` "direct path" (prepare_store / commit_store / cancel_store, `pending_extent[]`). | No such symbols in today's tree (`grep -rn "prepare_store\|commit_store\|cancel_store\|pending_writes"` finds none). Dispatcher spec 001 FR-020..FR-022 `~~REMOVED~~ (superseded 2026-07-02)`, `dispatcher/specs/001-dispatcher-cache-interface/spec.md:297-299`. This was dead code in the model. |
| 2 | **Removed** `ShutdownCleanup`. | It modelled `pending_writes.clear()`, which no longer exists. Shutdown today: evictors (dispatcher `lib.rs:2115-2121`), then writer drain (`lib.rs:2123-2125` → `background.rs:108-115`, drain loop `background.rs:131-137`), then checkpoint all extent managers (`lib.rs:2128-2137`). Now modelled at the tail of `init`. |
| 3 | `BgWriter` rewritten branch by branch from `process_write_job` (`lib.rs:1390-1465`). | Covers: mt.peek None → release_read (1399-1405); no-drives synthetic offset (1407-1412); DmaBuffer::from_raw Err (1422-1435); reserve_extent Err (1439-1445); write_buffer_to_ssd Err → handle drop → abort (1451-1456); `let _ = publish()` (1463); `let _ = convert_to_storage` (1464). `reserve_extent` has only one caller in the tree (`lib.rs:1439`). |
| 4 | **Added** the dispatch-map (`dm_loc/dm_off/dm_rref`). | Removing the dispatch-map from the model (orig README: "not relevant to extent lifecycle") hid the question the task asks: what the map points at after publish. `convert_to_storage` (dispatch-map `lib.rs:176-208`), `release_read` (267-285), `remove` (334-358), `try_evict_to_block` (548-577), and `promote_block_to_memory_tier` (482-523) are now modelled. |
| 5 | **Added** the extent free paths. | `IDispatcher::remove` (`lib.rs:3096-3139`), SSD evictor (`background.rs:313-349`), MT demote (`background.rs:531-537`, `lib.rs:1066-1071`), `remove_extent` → `remove_extent_by_offset` (extent-manager `lib.rs:680-684`, `region.rs:121-150`), and pending frees flushed by checkpoint (`lib.rs:313-320`, `region.rs:152-160`) → new state `PENDING_FREE`. |
| 6 | `publish_extent` now follows `publish_fn` (extent-manager `lib.rs:597-616`). | Covers the FREE_KEY (`u64::MAX`, `slab.rs:5`) branch, which frees the slot and returns Ok (600-606). Also covers an optional `Err` (`PUBLISH_ERR`), allowed by the type at `iextent_manager.rs:132-139` (publish takes `abort_fn` at l.137 before calling `f()`). |
| 7 | **New properties** P6 (every SSD offset in the map is a PUBLISHED extent of that key) and P7 (no published extent orphaned at shutdown). | P1-P5 kept. P2 kept as "legacy, structural" and asserted as "ABORTED is never a resting state". |
| 8 | Returned-value witnesses `last_reserve/last_publish/last_convert/last_release/last_rm_ext`, each set at the modelled code line, each with an assert. | Workbench rule 2d-6. |
| 9 | Parameters `N_CLIENTS=3` (keys), `N_EXTENTS=2`, `QUEUE_CAP=3`; evictor key-choice made atomic with its action. | Keeps the full (control) state space below 10M (9.2M). |
| 10 | Correspondence table, `.pml` header and inline comments: all re-derived by symbol. | Every orig citation was stale. For example, `process_write_job` was cited 350-431 (now 1390-1465), the direct path 1500-1663 (removed), and shutdown 808-819 (now 2112-2137). |
| 11 | Added `no_lost_extents_control.pml`: the same model plus `#define FIX_REMOVE 1`, `#define FIX_FREE_KEY 1`. | Control for the candidate bugs. |

The Makefile is unchanged.

## 2. Summary row

| Model | Status | Divergences | Verification | States | Unreached | Returned values (modelled / left out / missing) |
|-------|--------|-------------|--------------|--------|-----------|------|
| no-lost-extents | UPDATED | direct path + ShutdownCleanup removed (dead code); dispatch-map + free paths + FREE_KEY + publish result added; all lines re-derived | **FAIL** (P7, candidate bug) / control PASS | 1,747 (default, stopped at error) / 9,195,270 (control) | control: 3 states, all expected (see 3) | **22 / 9 / 0** |

The values are listed in README "Returned values" and "Left out".

## 3. Results

All runs: `/usr/bin/time -v`, `-m200000`, default hash `-w24`.

| Run | Command | Errors | States stored | Transitions | Depth | pan elapsed | Total mem (MB) | Wall | Max RSS |
|-----|---------|--------|---------------|-------------|-------|-------------|----------------|------|---------|
| **safety** | `make clean && make safety` | **1** (P7, depth 165) | 1,747 | 3,839 | 172 | 0 s | 138.97 | 2.19 s (incl. spin -a + cc) | 142,896 KB |
| **liveness** | `make liveness` | **1** (P7, depth 165) | 1,780 | 4,059 | 172 | 0 s | 138.97 | 2.51 s (incl. cc) | 142,904 KB |
| control safety | `no_lost_extents_control.pml`, `-DSAFETY` | 0 | 9,195,270 | 22,412,410 | 370 | 6.34 s | 899.62 | 6.41 s | 922,164 KB |
| control liveness | control, no `-DSAFETY`, `-a` | 0 | 9,476,851 | 23,750,093 | 370 | 6.40 s | 922.86 | 6.46 s | 946,232 KB |
| cex2 | `-DFIX_REMOVE=1` | 1 (P6 in convert_to_storage, depth 109) | 839,161 | 2,027,848 | 283 | 0.62 s | 208.21 | 0.65 s | 214,064 KB |
| cex3 | `-DFIX_REMOVE=1 -DFIX_FREE_KEY=1 -DPUBLISH_ERR=1` | 1 (P6, depth 111) | 800 | 1,659 | 173 | 0 s | 138.88 | 0.02 s | 142,900 KB |
| FIX_FREE_KEY only | `-DFIX_FREE_KEY=1` | 1 (P7, same as cex1) | 1,775 | — | 173 | 0 s | 138.9 | 0.02 s | 142,900 KB |

Unreached states:
- safety / liveness: pan stopped at the first error ("Search not completed"), so it printed no unreached list.
- control (safety and liveness, identical): `last_rm_ext = R_OFFSET_NOT_FOUND` in Client and in Evictor (good: once fixed, `remove_extent` is never given a bad offset), and `else -> skip` of the `read_ref > 0` guard in `convert_to_storage` (the job always holds the read ref). Nothing else.
- `make liveness`: pan warns "no accept labels are defined, so option -a has no effect". This model has no LTL/accept claim, so "liveness" here is the same assertion + invalid-end-state search without `-DSAFETY`. The orig model was the same; no progress or acceptance property was checked.

Logs: `safety.log`, `liveness.log`, `control_safety.log`, `control_liveness.log`, `trails/cex2_pan.log`, `trails/cex3_pan.log`, `trails/fixfreekey_only_pan.log`.
Trails: `no_lost_extents.pml.trail` (from the last `make liveness`), `trails/cex1_safety.trail`, `trails/cex1_liveness.trail`, `trails/cex2_free_key.trail`, `trails/cex3_publish_err.trail`. Replays: `trails/cex{1,2,3}_replay.txt` (`spin -t -p`, with the same -D flags).

## 4. Candidate bugs

### CANDIDATE BUG 1: `IDispatcher::remove` leaks the extent of a key whose write-through completed while it was still in DRAM (P7)

Trail `trails/cex1_safety.trail` (replay `trails/cex1_replay.txt`). Drives present (`no_drives = 0`), one key k:

1. `populate(k)` (`lib.rs:3143`) → `copy_gpu_to_memory_completed`: `create_memory_tier_entry` (`lib.rs:3508`, entry MemoryTier `ssd_offset=None`), `downgrade_reference` (`lib.rs:3552`, read_ref=1), enqueue `WriteJob` (`lib.rs:3559-3566`). [trail steps 35-36]
2. Client calls `remove(k)`. `dm.lookup(k)` returns `MemoryTier` → `block_offset = None` (`lib.rs:3105-3116`). [step 41]
3. BgWriter: `reserve_extent` → Ok (`lib.rs:1439`), write OK, `publish()` → Ok, slot PUBLISHED with key k (`lib.rs:1463`, extent-manager `lib.rs:609-614`). [steps 44-92]
4. BgWriter: `convert_to_storage(k, off)` sets `ssd_offset = Some(off)` and drops the read ref to 0 (`lib.rs:1464`, dispatch-map `lib.rs:184-196`). [step 109]
5. Client: `dm.remove(k)` succeeds, because read_ref = 0 (dispatch-map `lib.rs:334-358`); the entry is gone. `block_offset` is None, so `remove_extent` is **not** called (`lib.rs:3127-3133`). `remove` returns `Ok(())`. [step 113]
6. Shutdown, final checkpoint. The extent is still PUBLISHED with key k, and no dispatch-map entry references it. P7 is violated: neither `remove` nor the SSD evictor (`background.rs:321-349` iterates `dm.oldest_keys`) can ever free it.

**The race is not needed.** The same leak happens sequentially. After write-through finishes, the entry is `MemoryTier{ssd_offset: Some}`, so `remove`'s lookup returns `MemoryTier` → None → no `remove_extent`. This is exactly the sequence in `remove_existing_succeeds` (`dispatcher/src/lib.rs:6137-6150`: populate, `flush_to_ssd`, remove). That test runs without data drives, so it cannot see the leak. A promoted entry (BlockDevice → `promote_block_to_memory_tier` keeps `ssd_offset`, dispatch-map `lib.rs:482-507`) that is then removed leaks the same way.
- Effect: SSD space is lost for the lifetime of the run (`used_bytes` grows; the SSD evictor cannot see it). The extent is checkpointed as published, so a restart with `format_on_init=false` resurrects the removed key through `for_each_extent → recover_extent` (`lib.rs:1914-1925`).
- Spec: FR-009 (`spec.md:286`) and User Story scenario 2 (`spec.md:93`) only require freeing the extent "if in BlockDevice state". The code matches the spec's wording, but the spec misses the MemoryTier-with-`ssd_offset` state that FR-024 and write-through create, so the spec has the same gap.
- Minimal fix (control `FIX_REMOVE=1`): free the extent of **any** entry that has an SSD offset. Have `dm.remove` (or the lookup in `remove`) report `ssd_offset` for MemoryTier entries as well as the BlockDevice offset, under the map lock, and call `remove_extent` on it. With this fix and fix 2, the control passes: 0 errors, 9,195,270 states.
- Is it reachable in the real code? Yes, with ≥1 data drive and any populate → (write-through done) → remove. No assert was weakened.

### Candidate bug 2 (minor edge case): key `u64::MAX` gives the dispatch-map a dangling SSD offset (P6)

Found once bug 1 is fixed (`-DFIX_REMOVE=1`, `trails/cex2_free_key.trail`, replay `trails/cex2_replay.txt`). DFS reports bug 1 first in the default model, so this one does not show there.
1. `populate(u64::MAX)`. Nothing rejects the key: no `u64::MAX` or `FREE_KEY` check in dispatcher/dispatch-map/memory-tier src. [steps 31-32]
2. BgWriter: `reserve_extent(u64::MAX)` → Ok (`lib.rs:1439`). Write OK. `publish()` takes the FREE_KEY branch: `free_slot` and **return Ok** (extent-manager `lib.rs:600-606`, spec `extent-manager/.../spec.md:76-78, 258-262`). [steps 68-92, `last_publish = R_OK_DISCARDED`]
3. BgWriter: `convert_to_storage(u64::MAX, off)` stores `ssd_offset = Some(off)` for a slot that is now **free** (`lib.rs:1464`). [step 109] P6 fails.
- Effect if run on: the slot is reused for another key. Demoting `u64::MAX` and reading it serves the other key's data. A later `remove(u64::MAX)`/SSD eviction calls `remove_extent(off)`, which only checks that the slot is set and not FREE_KEY, not which key owns it (`region.rs:137`), so it frees the **other key's live extent**.
- Reachable only if a client uses CacheKey `u64::MAX`. Minimal fix (control `FIX_FREE_KEY=1`): reject `u64::MAX` in populate, or skip `convert_to_storage` (release_read instead) when `key == FREE_KEY`.

### Not a bug today (latent hazard): `let _ = write_handle.publish()` (lib.rs:1463)

You asked what happens if publish fails. Today's only implementation, `publish_fn`, has no `Err` path (extent-manager `lib.rs:597-616`), so `publish` never fails and this cannot happen now. With `PUBLISH_ERR=1` (`trails/cex3_publish_err.trail`), Spin shows what would happen if any implementation ever returned Err. `WriteHandle::publish` drops `abort_fn` *before* calling `f()` (`iextent_manager.rs:137-138`). The extent then stays RESERVED forever: it cannot be aborted, and `Drop` is a no-op. The dispatcher discards the Err and still calls `convert_to_storage` (`lib.rs:1464`), so the map points at an unpublished extent (P6, and P1 at shutdown). Suggested hardening: `if write_handle.publish().is_err() { release_read; return }`, and have `publish` run abort on `Err`. This is a design risk, not a candidate bug at acd24025.

## 5. Not checked or left out (honest list)

- See README "Left out" (9 returned values, each with file:line). The main ones: NotInitialized results, and DmaBuffer / write_buffer_to_ssd error *variants* (only their branches are modelled), plus checkpoint Err at shutdown (assumed Ok).
- Not modelled: multiple drives/extent managers (`drive_index` routing is assumed consistent between write and free: `lib.rs:581-590`, `background.rs:344-349`), memory-tier slot contents, `clear_memory_tier` as its own process, `batch_populate`, and restart/recovery (`format_on_init=false`).
- No LTL/progress property; "liveness" = the Makefile's `pan -a` without accept labels (see 3).
- The no-drives synthetic offset (`key*4096`, `lib.rs:1409`) is not checked against any extent manager (P6 guarded by `!no_drives`).
- Evictor candidate selection over-approximates `oldest_keys`. The lookup → remove in the SSD evictor stays two steps, as in the code, but each evictor step is otherwise atomic.
- I did not run the Rust code or tests. Bug 1's reachability was argued from the code and the existing `remove_existing_succeeds` test, not reproduced on hardware.
