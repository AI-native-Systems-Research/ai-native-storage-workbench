# J25 Part A — clear_memory_tier Spin model

- **Part A start:** 2026-10-21 05:25:01 UTC (job start; skill + code reading shared with Part B)
- **Part A end:** 2026-10-21 05:45:30 UTC (~20 min of the 1-hour box; all required items done)
- **Files:** `clear_memory_tier.pml` (faithful to current code),
  `clear_memory_tier_control.pml` (minimal fix + mutant arm), `Makefile`, `README.md`,
  `trail_a_original_property_a.pml.trail` (preserved copy of the (a) counterexample).

## What I modelled

Three actors over 2 keys, per the task: `CLR` = `clear_memory_tier`
(dispatcher lib.rs:3794-3825), `LKP` = a lookup that holds a dispatch-map read
pin on a MemoryTier entry (lookup_async MemoryTier hit, lib.rs:3036-3062), and
`WTR` = the background write-through writer (populate → copy_gpu_to_memory_
completed → WriteJob → process_write_job; lib.rs:3143-3207, 3489-3568,
1390-1465; background.rs:25-143). Dispatch-map and memory-tier operations are
atomic inlines with the interleaving windows the real locks leave open (dm op
and mt op are separate locks). Slot generations detect use-after-free/ABA.
Full citation tables: `README.md`.

## Result: the current code violates properties (a) and (b); (c) holds

**Finding (real code bug, not a modelling artifact).**
`clear_memory_tier`'s force-remove path (lib.rs:3816-3820) reads:

```rust
} else {
    // Entry not evictable (no SSD copy) — force remove.
    let _ = mt.remove(key);   // 3818: frees the DRAM slot FIRST, no pin check
    let _ = dm.remove(key);   // 3819: fails with ActiveReferences if pinned
}
```

The comment assumes `try_evict_to_block` fails only because "no SSD copy".
But the dispatch map has a second refusal — `ActiveReferences` (dm lib.rs:558-560,
"a held pin is its own variant") — and for that one the force-remove is unsafe:
`mt.remove` (mt lib.rs:483-514) frees the slot with no pin check, while a
pinned lookup (or the write-through writer, whose pin is the read_ref installed
by `downgrade_reference`, dm lib.rs:307-332) still points at the slot; the
following `dm.remove` then fails with ActiveReferences and the error is
swallowed by `let _ =`. This contradicts the code's own contract in
`evict_one_clean` (lib.rs:1041-1046: "this NEVER frees a slot that a pinned,
in-flight load still points at") and pins.rs:8-21.

**Counterexample, property (a)** (trail `trail_a_original_property_a.pml.trail`,
52 steps, violation at depth 51; in plain words):

1. Key 0 is MemoryTier, persisted, unpinned; key 1 MemoryTier persisted (init).
2. LKP looks up key 0, takes read pin (`rr[0]=1`) — dm.lookup, lib.rs:3011 / dm lib.rs:147.
3. LKP releases the pin (`rr[0]=0`) — release_read, lib.rs:3056.
4. LKP looks up key 0 again, takes the pin (`rr[0]=1`, snap=gen 0) — lib.rs:3036-3034.
5. CLR picks key 0; `try_evict_to_block` fails (ActiveReferences: `rr[0]=1`) — lib.rs:3814 → dm lib.rs:558.
6. LKP releases its pin in the window (`rr[0]=0`) — lib.rs:3056.
7. CLR executes `mt.remove(key)` — lib.rs:3818 → mt lib.rs:507-513: `slot[0]=false`,
   gen bumped. **At this instant the dispatch-map entry for key 0 is still
   MemoryTier pointing at the freed slot** → `assert(loc!=MT || slot)` fires.
   In the code, any `dm.lookup` that slips in between `mt.remove` and
   `dm.remove` (separate locks) observes the dangling MemoryTier entry and
   DMAs from freed DRAM (lookup path lib.rs:3048). The simplest trail needs no
   step 6 at all: with the pin still held, `dm.remove` at step 7 fails and the
   dangling state is permanent.

**Counterexample, property (b)** (same scenario continued, violation at depth 72;
in plain words): the entry stays MemoryTier-with-freed-slot (dm.remove refused,
lib.rs:3819 → dm lib.rs:341-343), so `mt.oldest_keys` no longer lists key 0,
the drain loop exits (lib.rs:3810-3811) and `clear_memory_tier` returns Ok
with key 0 still a warm MemoryTier entry pointing at released DRAM. Every
later lookup of key 0 serves freed memory (use-after-free), and the entry is
neither BlockDevice-with-offset nor gone.

**Property (c)** holds on the original: no actor releases a read pin it did
not take (all release sites are the pin taker's own `release_read` /
`convert_to_storage`; clear_memory_tier never touches read_ref).

## Control copy (must pass — it does)

`clear_memory_tier_control.pml` changes only the else-branch, mirroring
`evict_one_clean`'s discipline (dm transition before DRAM free; pinned
entries skipped):

```rust
} else {
    if dm.remove(key).is_ok() {   // succeeds only when unpinned
        let _ = mt.remove(key);   // slot freed only after the entry is gone
    }                             // pinned: skip, leave entry + slot intact
}
```

- Control, safety: **0 errors**, 552,471 states stored, 876,015 transitions,
  full search, 0.27 s, ~163 MB.
- Control, liveness (`[] (actors_done -> <> clr_done)`, acceptance cycles):
  **0 errors**, 561,755 states, 0.37 s.
- Control mutant (`-DMUTANT_RELEASE_PINS`: a plausible wrong fix that
  force-releases the outstanding pin so the remove succeeds): **rejected**,
  `assert(clrmypin)` at depth 51 — property (c) has rejection power.
- Original per-property: (a) violated at depth 51 (129 states on the error
  path); (b) violated at depth 72; (c) passes with 612,325 states, 0.31 s.

## Run protocol

Every pan run was memory-capped and time-limited as required:
`systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p
OOMPolicy=continue timeout 1800 ./pan… -m100000`; pan compiled with
`-DMEMLIM=16000 -DSAFETY` (liveness build without -DSAFETY, run with `-a`).

## How long each step took (approx., UTC)

- 05:25–05:34 skill + code reading (clear_memory_tier, dispatch-map,
  memory-tier, promote_and_serve, lookup/batch_lookup, background writer, pins.rs).
- 05:34–05:37 writing the two models.
- 05:37–05:40 original runs: (a) violated, trail replayed and preserved; (b) violated, trail replayed; (c) passes.
- 05:40–05:42 control build; first control run failed (b) — analysed trail,
  found it was a modelling artifact (concurrent re-store after drain), fixed
  the (b) formalization in both models (documented exemption).
- 05:42–05:44 re-runs: control 0 errors; original (b) still violated; (c)
  mutant rejected; liveness 0 errors.
- 05:44–05:45 Makefile, README, this report; `make control` / `make orig`
  re-verified end-to-end.

## Modelling choices that could hide a bug

1. **The store prologue is atomic** (mt.insert + create entry + downgrade +
   enqueue in one step, lib.rs:3506-3566). The real window where `write_ref=1`
   is visible (lookup blocks in `wait_for`, dm lib.rs:131-139) is not
   modelled; a bug living only in that window would be invisible here.
2. **`try_evict_to_block`'s three error variants are collapsed** into one
   `else`. The distinction matters in the code for counters only; no property
   here depends on it.
3. **Capacity is unbounded** — populate never triggers `evict_for_space`/
   `evict_one_clean`, so the interplay of clear with the *eviction* path is
   not covered (the safe order there is what the control copies).
4. **Property (b) needed a documented exemption**: an MT entry with a *live*
   slot at return is allowed (it must be a post-drain store, since clear
   returns only when the tier is empty). Weaker than the naive reading, but
   the naive reading is false in any concurrent system with stores; the
   original's real violation (MT with *no* slot) is still caught.
5. **One lookup actor, one writer actor, two keys, ≤2 pins** — races needing
   three simultaneous pins or a third key are out of scope.
6. **Cold path (BlockDevice serve) not modelled** — that is Part B.

## What is missing / not finished

Nothing on the required Part A list is missing: model with README tables,
three properties, safety + liveness runs, counterexample trails mapped to
code, control copy that passes, mutant that fails; `make control` and
`make orig` verified end-to-end. Not tested: NKEYS>2 and more than one
concurrent lookup actor (state space grows quickly; the found bug needs
neither).
