# J26 — `remove(key)` and the SSD extent

**Start:** 2026-10-21 19:14 UTC — **End:** 2026-10-21 19:52 UTC

## In short

No. `remove(key)` leaks the SSD extent whenever the entry is MemoryTier with a finished
write-through: `LookupResult::MemoryTier` hides `ssd_offset`, so remove never releases it
(P1/P4 fail). Remove can also double-release a stale offset after key reuse (P2) and steal a
pin via its NotExist `release_read` (P5). A minimal-fix control model passes all properties.

## The model

`modelling/spin/remove-ssd-extent/remove_ssd_extent.pml` — component-level, built from the
read-only snapshot (Certus @ acd24025). One key, one drive, 2 SSD offsets (reuse possible),
generation counter for key re-population. Initial state = the prompt's starting point: entry in
the memory tier, write-through job enqueued (populate finished through `downgrade_reference`).
Actors, concurrent: `RemoveClient` (remove ×2, lib.rs:3096-3136), `Populator` (populate,
lib.rs:3143-3199 + 3489-3569), `Writer` (process_write_job, lib.rs:1390-1465), `Demoter`
(evict_one_clean / MemoryTierEvictor, lib.rs:1050-1108, background.rs:471-575), `SsdEvictor`
(background.rs:279-370), `Lookup` (warm serve / cold promote, lib.rs:1360-1387, 900-903).
Each `atomic` block is one dispatch-map / extent-manager / memory-tier call (mutex-held in the
code); per-step file:line citations are in the model comments and the README Correspondence
table. Every pan run used the required `systemd-run … MemoryMax=60G … timeout 1800` wrapper and
was compiled with `-DMEMLIM=16000`. Spin 6.5.2.

### What was modelled

- The full `remove` protocol: `dm.lookup` (ref taken except NotExist; wait-for-writer
  abstracted as timeout), the unconditional `dm.release_read` of the `Ok(_)` arm, `dm.remove`
  (key-based gate requiring refs 0), `mt.remove`, `remove_extent` of the **lookup-captured**
  offset.
- Write-through: dequeue, `mt.peek`, `reserve_extent` (nondeterministic free-offset choice,
  OutOfSpace branch), injected `write_buffer_to_ssd` failure (abort + pin release), `publish`
  (extent instance identity tracked by (offset, serial)), `convert_to_storage` (sets
  `ssd_offset` on the still-MemoryTier entry, guarded ref decrement).
- Demotion (MT→BD keeps offset), cold-path promotion (BD→MT keeps offset), SSD evictor
  (capture offset → release ref → `mt.remove` → `dm.remove` gate → `remove_extent`),
  re-population of the key after removal.

### What was left out

Blocking condvar semantics (modelled as timeout-or-later-interleaving), the integrity-check
window between `create_memory_tier_entry` and `downgrade_reference` (a `set_checksum` failure
there leaves `write_ref=1` forever, lib.rs:3520-3550), multi-key interference, DRAM
capacity/`evict_for_space`, shutdown/recovery (`for_each_extent` would resurrect leaked extents
as zombie BlockDevice entries after restart), extent-manager internals, `MismatchSize`,
batch/remote paths. The dispatcher spec tree was absent from the snapshot; assertions derive
from the interface contracts plus the code's own stated ownership rules (README "Specification
basis").

## Results (BASE = code as-is)

| Run | Result | States stored | Time |
|-----|--------|--------------|------|
| base (all properties) | **FAIL** (P1 first) | 553 (stopped at error) | <0.1 s |
| P1 isolated | **FAIL** | 509 | 0 s |
| P2 isolated | **FAIL** | 190,626 | 0.12 s |
| P3 isolated | PASS (full search) | 2,324,957 | 1.71 s |
| P4 isolated | **FAIL** | 549 | 0.01 s |
| P5 isolated | **FAIL** | 3,460 | 0 s |
| **control** (minimal fix) | **PASS** (full search, 0 errors) | 1,169,588 (2.07 M transitions) | 0.81 s, 230 MB |
| **mutant** (control + `INJECT_STALE_OFFSET`) | **FAIL** (P2) | 190,006 | 0.12 s |
| liveness (control, `-DNP`, `pan -l`) | PASS — 0 non-progress cycles | 2,782,037 (3.95 M visited) | 3.28 s |
| extra: DRAM hazard (`make dram`) | **FAIL** | 166,528 | 0.1 s |

### P1 — after remove returns Ok, the extent is released or was never allocated — **FAIL: code bug**

Trail (trail-p1.txt, in plain words): the Writer completes the write-through of the initial
entry — `reserve_extent` → write ok → `write_handle.publish()` (lib.rs:1463) →
`dm.convert_to_storage` (lib.rs:1464) sets `ssd_offset = Some(0)` on the entry **which remains
MemoryTier** (dispatch-map lib.rs:184-186; only demotion flips it to BlockDevice). The client
calls `remove(key)`: `dm.lookup` returns `LookupResult::MemoryTier { pointer, size }` — the
variant has **no `ssd_offset` field** (dispatch-map lib.rs:156-162) — so `block_offset = None`
(lib.rs:3110-3113). `dm.remove` succeeds (refs are 0), `mt.remove` frees the DRAM slot, and the
`if let Some(offset) = block_offset` at lib.rs:3127 does nothing. remove returns `Ok(())` with
offset 0 still allocated. **Verdict: code bug.** A hot, persisted entry that is removed from
the memory tier always leaks its SSD extent; after a restart, dispatch-map `initialize`
(dispatch-map lib.rs:100-118) even resurrects it as a zombie BlockDevice entry via
`for_each_extent`.

### P2 — no extent is released twice — **FAIL: code bug (TOCTOU)**

Trail (trail-p2.txt): (1) write-through finishes for the initial entry (offset 0); (2) the
Demoter demotes it to BlockDevice; (3) the SsdEvictor's `get_evictable_offset` captures offset
0 (background.rs:322) and releases its ref; (4) the client's `remove` #1 `dm.lookup` also
captures `BlockDevice{0}` (lib.rs:3105-3109); (5) the evictor's `dm.remove` gate passes and it
releases offset 0 (background.rs:331-348); (6) the key is re-populated (generation 1, new
write job); (7) the writer's SSD write for generation 1 fails, so it releases the new entry's
pin (lib.rs:1451-1456) — the new entry is unpinned; (8) `remove` #1 resumes: its `dm.remove`
(lib.rs:3119) succeeds on the **replacement entry** (it holds no pin on it), then it releases
the **stale** offset 0 (lib.rs:3127-3131) — `remove_extent(0)` on an already-freed extent.
**Verdict: code bug.** The offset is captured at lookup time but the `dm.remove` gate is
key-based and can pass on a different entry instance; nothing re-validates the offset. The
evictor has the identical window (background.rs:322 → :331 → :348). Requires key reuse, which
a long-running cache performs routinely. With offset reuse instead of a write failure, the same
window frees a live extent (caught by P3's stronger 2-key reading, unmodelled here).

### P3 — no extent owned by two keys — PASS

With a single key this reduces to "the entry's claimed offset is allocated", which held over
the full 2.3 M-state search. The stale-release-with-reuse hazard (freeing an extent another
live entry claims) needs ≥2 keys; the same TOCTOU is already demonstrated by P2. Honest
limitation, not an acquittal.

### P4 — at the end, every allocated extent belongs to an entry — **FAIL: code bug**

Same root cause as P1 observed at end-of-run (trail-p4.txt): extent 0 allocated, no entry owns
it. Additional reachable variant (chained from the P5 theft): a job whose pin was stolen
publishes an extent after its entry was removed (`convert_to_storage` → KeyNotFound, ignored,
lib.rs:1464) — an orphan extent no one will ever free.

### P5 — remove never releases a pin it did not take — **FAIL: code bug**

Trail (trail-p5.txt): remove #1 removes the initial entry; remove #2's `dm.lookup` returns
`NotExist` — **no read ref is taken** for NotExist (dispatch-map lib.rs:142-144) — but the
`Ok(_)` arm still calls `dm.release_read(key)` (lib.rs:3110-3112). The key is re-populated in
between (generation 1 holds the write job's pin, `read_ref = 1`), so remove #2's release
decrements the **job's pin** to 0. **Verdict: code bug.** Consequences: `dm.remove` can now
succeed while the write-through is in flight → the DRAM slot is freed under the writer and the
published extent becomes an orphan (P4 variant above).

### Control (minimal fix) — passes

Two changes, both local to the release protocol: (1) the offset to free is read **atomically
with `dm.remove`** (i.e. `IDispatchMap::remove` returns the removed entry's `ssd_offset`,
taken under the dispatch-map lock) and both the dispatcher's remove and the SSD evictor release
that offset — fixes P1, P2, P4; (2) remove does not call `release_read` after a `NotExist`
lookup — fixes P5. Control: 0 errors, full search, 1.17 M states. Unreached states in the
control build are exactly the violation arms plus the OutOfSpace arm (unreachable when nothing
leaks) — expected pruning.

### Mutant — fails as required

`INJECT_STALE_OFFSET` on top of the control regresses only the remove-side fix (release the
lookup-captured offset when lookup saw BlockDevice). The mutant fails with the P2 double
release (trail-mutant.txt), showing the properties can catch this class of regression.

### Liveness

`pan -l -DNP` on the control build: 0 non-progress cycles (every cycle passes
`progress_job`; the writer drains the queue, no starvation). Liveness is near-structural here
because all actor loops are bounded — real unbounded loops (evictor sleep loop, condvar waits)
are left out; see below.

### Extra finding (outside P1-P5, `make dram`)

The SSD evictor frees the DRAM slot **before** its `dm.remove` gate (background.rs:328 vs :331).
Trail (trail-dram.txt): evictor captures a BlockDevice entry's offset; the entry is removed and
the key re-populated in the window; the evictor's `mt.remove(key)` then frees the **new live
MemoryTier entry's** DRAM slot, and its subsequent `dm.remove` fails (the new job's pin) so it
`continue`s — leaving a live entry pointing at freed DRAM. Same key-based-TOCTOU family as P2.
Also unmodelled but noted from code reading: the integrity-check failure that leaves
`write_ref = 1` forever (lib.rs:3520-3550).

## Modelling errors found and fixed during the runs (kept for the record)

Three counterexamples against the *control* build were modelling errors, not code bugs — each
was fixed and everything re-run: (1) P1 compared offset values instead of extent instances,
firing when a correctly-freed offset was reused before remove's return; (2) the Lookup actor
could take a read ref on a **nonexistent** entry (a guard on stale `loc` was executable when
`entry_exists = 0`); (3) lookups could succeed during `write_ref = 1` (the real `dm.lookup`
blocks on the condvar) and `downgrade` assigned `read_ref = 1` instead of incrementing
(dispatch-map lib.rs:318-322). One base-build P2-style trail was re-verified against the code
before being declared a code bug.

## Modelling choices that could hide a bug

- The condvar wait in `dm.lookup`/`take_read` is abstracted as an immediate timeout; the
  successful path only appears as "runs later". Real-time waits, deadline exhaustion and the
  write_ref-forever leak are invisible.
- One key: cross-key extent aliasing (P3's stronger reading) and LRU-ordering effects are
  untested.
- Bounded attempts (2 removes, 1 re-populate, 1 demote/evict/lookup, 2 jobs): chains needing a
  third generation are unexplored.
- Each dm/em/mt call is atomic: a race *inside* one call (the real lock is held, but the model
  cannot see internal ordering) is invisible by construction.
- The Populator collapses reserve→D2H→create into one atomic and omits the integrity-check
  failure between create and downgrade.
- `mt_slot` is a single boolean (no DRAM slot identity per generation); the DRAM hazard is only
  checked at the evictor's `mt.remove` site.
- pan stops at the first error per run; the per-property `make props` runs mitigate this.
- Extent manager is 2 uniform offsets; allocator internals (regions/slabs, `FREE_KEY`
  sentinel handling) are abstracted.

## Refused commands

- A placeholder `for … done` loop (a no-op guard I issued before running the make targets) —
  refused by the permission mode; not needed, work continued with direct `make` calls.
- A compound `spin … | grep -B3 -A1 … ; cp … ; echo saved` command — refused; re-issued as
  separate single-purpose commands (redirect to file, then grep), which succeeded.
- `./pan-live -h` run directly (without the systemd-run wrapper) — refused; re-run through the
  required wrapper, which succeeded.

## Step timing (UTC)

19:14 start, code reading (dispatcher remove / background.rs / dispatch-map / extent-manager /
interfaces) 19:14-19:33; model + Makefile written 19:33-19:35; first base run 19:35; iteration
on three model bugs 19:35-19:45 (control green at 19:45); all base/props/mutant/liveness/dram
runs final by 19:48; README + this report 19:48-19:52. Total ≈ 38 minutes of the 2-hour box.
