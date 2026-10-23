# remove(key) vs. the SSD extent lifecycle

> **What it checks:** remove frees the SSD space of the entry, releases only its own read reference, and no extent is freed twice.
> **Result:** the model fails (P1, P2, P4, P5); its control copy (-DCONTROL in the same file) passes.
> **The bugs:** root causes #9 and #12 (remove leaks the SSD extent; releases a foreign pin), repaired by GLM-5.3, not merged yet. P2 (an extent freed twice) is not repaired yet.
> **Certus commit:** `acd24025`.
> **History:** written from scratch in J26 (GLM-5.3, 2026-10-21).

## Scope

Single component (dispatcher + dispatch-map + extent-manager abstraction), modelled from
`code/components/dispatcher/src/lib.rs` at commit `acd24025`. The model answers: when
`IDispatcher::remove(key)` deletes an entry that is in the memory tier **and** already has a
finished write-through (an `ssd_offset`), is the SSD extent always released? Can a removed key
leave an extent no entry owns (leak), or can an extent be released twice?

One cache key, one drive / extent manager, `N_EXT = 2` distinct SSD offsets (so an offset can be
freed and reused), and a generation counter that separates entry instances created by
re-population. Initial state: `populate(key)` has finished through `downgrade_reference`; the
write-through `WriteJob` is enqueued but not yet processed — i.e. the prompt's starting point
"memory tier, write-through in flight/finished".

## Properties Verified

| ID  | Property (as asserted in the model) | Type | Result (BASE) | Result (CONTROL) |
|-----|-------------------------------------|------|---------------|------------------|
| P1  | After `remove` returns Ok, the extent **instance** (offset, serial) the removed entry owned is released or was never allocated (`remove_ssd_extent.pml:241`) | Safety | **FAIL** (leak) | PASS |
| P2  | No `remove_extent` call on an extent that is not currently allocated (double release) (`:113`) | Safety | **FAIL** (stale-offset TOCTOU) | PASS |
| P3  | The entry's claimed SSD offset is currently allocated (`:83`, plus end check in `init`) | Safety | PASS | PASS |
| P4  | At the end, every allocated extent is claimed by the entry; no dangling reservation; queue drained (`:504-522`) | Safety | **FAIL** (leak) | PASS |
| P5  | `remove` never releases a read pin it did not take: its `release_read` must hit the same entry generation its `lookup` observed (`:175`); no pin left held at end (`:531`) | Safety | **FAIL** (NotExist-arm pin theft) | PASS |
| LV  | No non-progress cycle: any infinite run keeps completing write jobs (`progress_job`, `pan -l -DNP`) | Liveness | — (stops on safety) | PASS |

Extra (outside P1-P5, run with `make dram`): the SSD evictor frees the DRAM slot *before* its
`dm.remove` gate, which can leave a live promoted MemoryTier entry pointing at freed DRAM
(`background.rs:322-333`). **FAIL** on BASE.

## System Abstraction (actors, all concurrent)

| Real thread / caller | Promela process | Modelled from |
|----------------------|-----------------|---------------|
| Client calling `remove(key)` (twice) | `RemoveClient` | dispatcher `lib.rs:3096-3136` |
| Client store path `populate(key)` (once, optional) | `Populator` | `lib.rs:3143-3199`, `3489-3569` |
| Background writer thread (write-through) | `Writer` | `lib.rs:1390-1465`, `background.rs:117-143` |
| Memory-tier demotion (`evict_one_clean` / `MemoryTierEvictor`) | `Demoter` | `lib.rs:1050-1108`, `background.rs:471-575` |
| Background SSD evictor (one attempt) | `SsdEvictor` | `background.rs:279-370` |
| One client lookup/load (warm serve or cold promote) | `Lookup` | `lib.rs:1360-1387`, `900-903` |

The dispatch map is modelled as shared variables (`entry_exists`, `loc`, `has_ssd`, `ssd_off`,
`read_ref`, `write_ref`, `gen`); each `atomic` block is one dispatch-map / extent-manager /
memory-tier call (these hold a mutex in the real code). The extent manager is modelled as two
offsets with `ext_owned` / `ext_resv` / `ext_serial` (serial = extent instance identity, since
offsets are reused).

## Returned values

| Model outcome | Meaning in the code |
|---------------|---------------------|
| `r_removed = true`, P1 check passes | `remove` returns `Ok(())` (`lib.rs:3135`) |
| `r_removed = false` | `remove` returns `Err(DispatcherError::KeyNotFound)` (`lib.rs:3120`, from `DispatchMapError::KeyNotFound` / `ActiveReferences`) |
| `r_gen == NOGEN` (255) | `dm.lookup` returned `LookupResult::NotExist` (no read ref taken, dm `lib.rs:142-144`) |
| `r_gen == ABORT` (254) | `dm.lookup` timed out waiting for `write_ref == 0` → remove returns Err (`lib.rs:3114`) |
| `r_bd = true`, `r_off = offset` | `dm.lookup` returned `LookupResult::BlockDevice { offset }` (`lib.rs:3106-3109`) |
| `r_bd = false`, `r_off = NOOFF` | `dm.lookup` returned `LookupResult::MemoryTier` — **`ssd_offset` is not exposed by this variant** |
| `l_kind` 0/1/2/3 | lookup returns NotExist / timeout / MemoryTier / BlockDevice |
| `w_fail = true` | `write_buffer_to_ssd` error → pin released, `WriteHandle` drops → abort (`lib.rs:1451-1456`) |
| `w_ok = false` at reserve | `reserve_extent` OutOfSpace → pin released, abort (`lib.rs:1441-1444`) |
| `ok = false` in Populator | `create_memory_tier_entry` `AlreadyExists` → populate fails (`lib.rs:3510-3513`) |

## Correspondence

| Model location | Intended behavior (spec / interface contract) | Implemented behavior (source, commit acd24025) |
|----------------|----------------------------------------------|------------------------------------------------|
| `RemoveClient` step 1, `:152` (`r_off = NOOFF` for MemoryTier) | remove should free *all* resources the entry owns, incl. its SSD copy | `lib.rs:3105-3115`: only the `BlockDevice{offset}` arm captures an offset; `LookupResult::MemoryTier` (interfaces `idispatch_map.rs`) has no `ssd_offset` field, so a MemoryTier entry with a finished write-through is freed from DRAM but its extent is never released — **finding (leak)** |
| `RemoveClient` step 2, `:164-175` | a release must match an acquire | `lib.rs:3110-3112`: the `Ok(_)` arm calls `dm.release_read` even after `NotExist`, which took no ref (dm `lib.rs:142-144`) — **finding (pin theft)** |
| `RemoveClient` step 3/5 (`r_rel = r_off` in BASE) | free exactly what was removed | `lib.rs:3105-3131`: offset captured at lookup, released after `dm.remove` with no re-validation — **finding (TOCTOU)** |
| `dm_convert_to_storage`, `:91-97` | write-through records the SSD copy on the entry | dm `lib.rs:176-208`: sets `ssd_offset` on a *MemoryTier* entry and decrements the job's read ref |
| `Writer` publish + convert, `:461-463` | reserve → write → publish → attach | `lib.rs:1439-1464` |
| `Writer` failure arms, `:446-457` | failures release the pin and abort the reservation | `lib.rs:1441-1444`, `1451-1456`; `WriteHandle::drop` → abort (interfaces `iextent_manager.rs:149-155`) |
| `SsdEvictor` `:329-334` (get, release, mt.remove, dm.remove, release extent) | evictor removes cold entries and frees their extent | `background.rs:322-348` — same TOCTOU as remove: offset captured before the `dm.remove` gate (`:331`) |
| `Demoter` `:297-306` | demotion keeps the data resolvable | dm `lib.rs:548-580` (`try_evict_to_block` keeps `ssd_off`) |
| `Lookup` cold path `:404-411` | promotion retains the SSD offset | dm `lib.rs:482-528` (`promote_block_to_memory_tier`, `:502-508`) |
| `Populator` `:264-281` | store creates the entry, downgrades, enqueues write-through | `lib.rs:3159-3198`, `3508`, `3552`, `3561` |
| `ext_release`, `:108-119` | freeing an unknown offset is an ignored error | extent-manager `lib.rs:680-684` returns `OffsetNotFound`, callers use `let _` |

## Assumptions / Stubs

- One key, one drive: `drive_index(key, n)` is always 0; the remove/evictor/write paths all use
  the same extent manager, so no cross-drive confusion is modelled.
- The dispatch-map mutex/condvar: each dm call is one atomic block. `dm.lookup`'s *wait* for
  `write_ref == 0` is abstracted as a nondeterministic immediate Timeout (dm `lib.rs:128-139`);
  the successful path is covered by interleavings that run after `downgrade_reference`.
- SSD evictor utilization/threshold/`oldest_keys` logic (`background.rs:289-314`) is abstracted
  to a nondeterministic "run one eviction attempt or not".
- `mt.peek`/`mt.get`/`mt.remove` are a single `mt_slot` boolean; DRAM data content is not modelled.
- The write job's payload is empty (jobs are indistinguishable tokens); `WriteJob` channel is
  bounded at 2 (max jobs in the scenario), the real channel is unbounded.
- `write_buffer_to_ssd` failure is a nondeterministic branch (skill rule: inject failure
  branches), as is reserve OutOfSpace.

## Left out (see also REPORT.md "modelling choices that could hide a bug")

- The integrity-check section between `create_memory_tier_entry` and `downgrade_reference`
  (`lib.rs:3520-3550`): a `set_checksum` failure there returns Err **leaving `write_ref = 1`
  forever** — every later `dm.lookup`/`remove` on that key times out. Not modelled (would need
  the wait semantics, not the timeout abstraction).
- The condvar *blocking* semantics of `dm.lookup`/`take_read` (real waits, deadlocks, the 2 s
  timeout budget). Only the timeout outcome is modelled.
- `MismatchSize` lookups, `batch_lookup`, remote lookup, shutdown/checkpoint/recovery
  (`for_each_extent` re-inserts orphan extents after restart — a leaked extent would resurrect
  as a zombie BlockDevice entry), multi-key interference, DRAM capacity/`evict_for_space`,
  `convert_memory_tier_to_block`'s take_write protocol, metrics.
- Extent-manager internals (regions/slabs/buddy allocator): offsets are uniform slots; `publish`
  keyed by the job's key is not tracked (extent identity is (offset, serial) in the model).
- P3's stronger reading ("an extent owned by two keys") needs ≥ 2 keys; with 1 key P3 reduces to
  "the entry's claimed offset is allocated", which holds in BASE. The stale-release-with-offset-
  reuse hazard is instead caught by P2 (release of a free extent) in the non-reuse interleaving.

## Specification basis

The dispatcher spec tree (`components/dispatcher/specs/`) was **not present** in the read-only
snapshot (only `src/` was provided), so the assertions are derived from the interface contracts
in `code/components/interfaces/` (`idispatch_map.rs`, `iextent_manager.rs`) plus the resource-
ownership reading of the code comments themselves (e.g. `lib.rs:1035-1046` states demotion
"keeps the data on SSD"; the evictor comment `background.rs:342-344` states the free must match
the write path's placement). Gap: no spec clause states who owns an SSD extent while its entry
is still MemoryTier — the model takes the stronger safe interpretation (the entry owns it from
`publish` on), which is what `for_each_extent`-based recovery assumes.

## Mutant

- Switch: `INJECT_STALE_OFFSET` (on top of `CONTROL`). It regresses the control fix on the
  remove side only: when lookup saw `BlockDevice`, remove releases the **lookup-time** offset
  instead of the offset read atomically at `dm.remove` — the historical TOCTOU.
- Expected: CONTROL = 0 errors; MUTANT = ≥ 1 assertion violation (P2 double release observed).
- The BASE build itself is the "code as-is" build and fails P1/P2/P4/P5 (see REPORT.md).

## Running

```sh
make            # base (code as-is): expect P1 violation
make control    # minimal fix: expect 0 errors
make mutant     # control + INJECT_STALE_OFFSET: expect P2 violation
make props      # base with each property isolated (own trail per property)
make liveness   # control + pan -l -DNP: non-progress cycles
make dram       # extra: evictor DRAM-slot TOCTOU (outside P1-P5)

# replay a trail (match the -D flags of the build that produced it), e.g.:
spin -t -p -g -l -DDISABLE_P2 -DDISABLE_P3 -DDISABLE_P4 -DDISABLE_P5 remove_ssd_extent.pml
```

Every `pan` run goes through
`systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p OOMPolicy=continue timeout 1800`
and is compiled with `-DMEMLIM=16000`, per the workbench rules.

## Tuning the Model

- `N_EXT` (2): number of distinct SSD offsets. 2 is the minimum that allows freed-offset reuse.
- RemoveClient `calls < 2`, one optional Populator run, one Demoter/SsdEvictor/Lookup attempt,
  `jobq` capacity 2: these bound the scenario to "one remove of the initial entry, one
  re-populate, one remove of the replacement" — enough for every reported counterexample; the
  full state space is ~2.3 M states (largest completed run, P3) and verifies in seconds.
- Increasing key count or attempts would be needed to test multi-key extent aliasing (P3's
  stronger reading).
