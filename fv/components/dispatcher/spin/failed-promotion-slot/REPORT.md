# J25 Part B report — failed cold promotion and its memory slot

**Part B start:** 2026-10-21 05:46 UTC (immediately after Part A's status line)
**Part B end:** 2026-10-21 06:28 UTC (this report; STATUS and SUMMARY follow)

> ### In short
> No. On a failed SSD read both promotion paths return Err without releasing
> the reserved memory-tier slot (lib.rs:882-884, 2710); the slot leaks
> permanently (P1, P5, liveness fail) and later lookups get
> AlreadyExists→Err (P3). The dispatch-map entry itself stays correct
> (P2, P4 hold). One-line fix verified.

## 1. The question

> When the promotion of a cold (SSD) entry to the memory tier fails (for
> example the SSD read fails), is the reserved memory slot always released,
> and does the dispatch-map entry stay correct?

**Answer: the slot is NOT always released — this is a code bug. The
dispatch-map entry itself stays correct.** All paths below are in
`code/components/dispatcher/src/lib.rs` unless marked otherwise.

## 2. What was modelled

`B/failed_promotion_slot.pml` (original) and `B/failed_promotion_slot_control.pml`
(minimal fix + mutants), 2 keys, capacity 2. Actors:

- `LKP(1)` — `lookup_async` → `promote_and_serve` (lib.rs:3063-3065 classify,
  750-908 promote) and the `batch_lookup` pooled-cold arm (2228-2994,
  2428-2443, 2553-2582, 2664-2714), folded into one actor: classify (pin
  taken and released, dm lib.rs:128-174/267-285), reserve via
  `evict_and_insert` (1176-1224 → mt.insert, memory-tier lib.rs:328-390),
  pipelined SSD read, flip via `dm.promote_block_to_memory_tier`
  (dispatch-map lib.rs:482-528). Its SSD read fails.
- `LKP(2)` — a second lookup of the same cold key with a healthy SSD, started
  either concurrently or after LKP(1) returned. Its `AlreadyExists` arm models
  both the single-key behaviour (no recovery, lib.rs:789) and the batch
  recovery pass (2937-2946 → `serve_concurrently_promoted` 1349-1388, bounded
  wait then `Err(KeyNotFound)` 1372-1373).
- `EV` — the eviction path that frees memory: `MemoryTierEvictor`
  (background.rs:526-554) / `evict_one_clean` (lib.rs:1050-1108), i.e.
  `dm.try_evict_to_block` (dm lib.rs:548-580) then `mt.remove`, unbounded
  rounds.
- `FIN` — end-state checks once both lookups returned.

State: dispatch-map `loc`/`persisted`/`rr` (read pins), memory-tier
`slot`/`gen` (generation counter = ABA detector)/`slot_bad` (content came
from a failed read). Full tables in `B/README.md`.

## 3. Results

Every pan run: `systemd-run --user --scope -q -p MemoryMax=60G
-p MemorySwapMax=0 -p OOMPolicy=continue timeout 1800 ./pan… -m100000`,
compiled with `-DMEMLIM=16000`. All searches ran to completion (or to first
error) in ≤ 0.03 s; every build's total pan memory was ~134 MB (128 MB hash
table −w24 + 5.3 MB DFS stack), far under the caps.

### Original (current code)

| Run | Result | States | Time |
|-----|--------|--------|------|
| safety, all properties | **P1 violated**, depth 43 | 299 stored (stopped at error) | 0 s |
| `-DNO_P1` | **P5 (orphan slot) violated**, depth 48 | 304 | 0 s |
| `-DNO_P1 -DNO_P5` | **P3(ii) violated**, depth 48 | 614 | 0 s |
| `-DNO_P1 -DNO_P3` | **P5 (orphan slot) violated**, depth 46 | 302 | 0 s |
| `-DNO_P1 -DNO_P3 -DNO_P5` | 0 errors — **P2 and P4 hold** (full search) | 8,071 stored | 0.01 s |
| liveness (`-DLIVENESS -DNO_P1 -DNO_P3 -DNO_P5`, `pan -a -f`) | **acceptance cycle**, depth 97 — permanent orphan | 417 stored / 777 visited | 0 s |

### Control (minimal fix: `mt.remove(key)` in the failed-read branch, mirroring lib.rs:3770-3771)

| Run | Result | States | Time | Memory |
|-----|--------|--------|------|--------|
| safety (`-DSAFETY`) | **0 errors** (full search) | 11,885 stored / 21,578 transitions | 0.01 s | 134.2 MB |
| liveness (`pan -a -f`) | **0 errors** (full search) | 15,845 stored / 35,076 visited | 0.03 s | 134.6 MB |

### Control mutants (rejection tests)

| Mutant | Expected | Observed |
|--------|----------|----------|
| `-DMUTANT_FLIP_BAD_SLOT` (wrong fix: keep slot, flip anyway) | rejected by P4 | **rejected**, `!(slot_bad[k])`, depth 43 |
| `-DMUTANT_DOUBLE_FREE` (raw second deallocation) | rejected by P2 | **rejected**, `slot[k]`, depth 44 |
| `-DMUTANT_DOUBLE_REMOVE` (fix's `mt.remove` twice) | initially expected to pass | **rejected**, invariant `loc!=MT \|\| slot`, depth 48 — see §6 |

Counterexample trails preserved: `trail_b_original_p1/p3/p5/liveness.pml.trail`,
`trail_b_control_MUTANT_*.pml.trail`.

## 4. The bug, in plain words (trails mapped to code)

### P1 — the reserved slot is not released (trail_b_original_p1, 44 steps)

1. Start: key 0 is cold (dispatch-map `BlockDevice` entry, data on SSD); key 1
   is warm (`MemoryTier`, persisted, unpinned).
2. `EV` runs one eviction round: key 1 is unpinned and persisted →
   `try_evict_to_block` demotes it to `BlockDevice` (dm lib.rs:548-580) and
   `mt.remove` frees its slot (background.rs:531-537). Tier now empty.
3. Both lookups classify key 0: `dm.lookup` → `BlockDevice`; each takes a read
   pin and releases it immediately (lib.rs:3063-3065 single-key /
   2428-2429 batch).
4. `LKP(1)` reserves a slot: `evict_and_insert` → `mt.insert` Ok
   (lib.rs:767-790 / 2557-2558; memory-tier lib.rs:375-386).
5. `LKP(2)` also tries to reserve: `mt.insert` → `AlreadyExists`
   (memory-tier lib.rs:356-358) — it lost the race; the single-key path has
   no recovery (lib.rs:789) so it returns Err.
6. `LKP(1)`'s pipelined SSD read into the reserved slot fails
   (lib.rs:870-884 / 2664-2673).
7. `return Err(e)` — **lib.rs:882-884 (single-key) and lib.rs:2710 (batch):
   the error is returned without any `mt.remove`.** P1 fires: the slot this
   lookup reserved (its own generation) is still allocated. Final state:
   `slot[0]=1` while `loc[0]=BD`.

The reference correct behaviour exists in the same file:
`promote_to_memory_tier` does `mt.remove(entry.key)` on exactly this failure
(lib.rs:3770-3771). The pooled-cold paths simply omit it. The same leak shape
exists at three more batch sites (create_stream failure lib.rs:2618-2627,
channel checkout failure 2645-2651, no-drives promote failure 2497-2506) —
noted by inspection, not separately modelled.

**Verdict: code bug** (re-checked against the source; the model gives the
code every escape — P1 exempts a concurrent re-insert of the key by checking
the lookup's own reservation generation, so only a genuine leak of this
lookup's slot fires it).

### P3 — the entry stays correct, but cannot be read again (trail_b_original_p3, 49 steps)

Same prologue: `LKP(1)` reserves, its read fails, it returns Err without
`mt.remove` (lib.rs:882-884) — the leak happens (P1 disabled in this
attribution run).

1. `LKP(1)` finishes: `l1_done=1`.
2. `LKP(2)` now starts — *after* the failed promotion returned.
3. `LKP(2)` classifies key 0: `dm.lookup` → `BlockDevice` — **the entry
   itself is still correct** (P3(i) passes in this run: no flip happened).
4. `LKP(2)` tries to reserve: `mt.insert` → **`AlreadyExists`** — the leaked
   slot still holds key 0 (memory-tier lib.rs:356-358).
5. Single-key arm: no recovery (`Err(e) => return Err(e)`, lib.rs:789) →
   `LKP(2)` returns Err. (The batch arm fails too: `serve_concurrently_promoted`
   waits for a MemoryTier transition that never comes — the insert winner is
   the lookup that failed — and times out → `Err(KeyNotFound)`,
   lib.rs:1372-1373.)
6. FIN: P3(ii) fires — a lookup that started after the failed promotion
   returned cannot be served.

**Verdict: code bug**, a direct downstream consequence of the P1 leak
(the entry is never corrupted; it is the *slot map* that is poisoned).

### P5 and liveness — the leak is permanent (trail_b_original_p5, 47 steps; trail_b_original_liveness, 98 steps)

Same prologue. FIN's end-check fires: `slot[0]=1` while `loc[0]=BD` — a live
memory-tier slot with no `MemoryTier` entry. The liveness run shows all
processes terminate in that state and the never claim cycles there forever
(acceptance cycle from depth 97): nothing can ever reclaim the slot —

- the demotion evictors refuse it: `try_evict_to_block` requires an MT entry
  (dm lib.rs:548-580; `evict_one_clean` skips on error, lib.rs:1066-1071;
  MemoryTierEvictor likewise, background.rs:531-537), and
- every future `mt.insert` of this key returns `AlreadyExists` forever
  (memory-tier lib.rs:356-358).

So the DRAM reservation is leaked for the lifetime of the process, and with
`CAP` slots the pool can eventually be exhausted entirely (each failed cold
read of a distinct key permanently removes one slot). **Verdict: code bug.**

### What holds on the original

- **P2** (no slot released twice): the slots-map remove returns the slot once
  and a second remove is a `KeyNotFound` no-op (memory-tier lib.rs:507-510);
  no path double-frees.
- **P4** (never serve garbage): `promote_block_to_memory_tier` is only called
  after a successful read (lib.rs:903 / 2681-2691); the failed-read slot is
  never flipped, so the stale `BlockDevice` entry (correct data on SSD) is
  what remains.

## 5. The control (minimal fix) and its mutants

The fix is one line in each failed-read arm — release the reservation before
returning the error, exactly what `promote_to_memory_tier` already does
(lib.rs:3770-3771):

```rust
if let Some(Err(e)) = read_result {
    let _ = self.memory_tier.remove(&key);   // <-- the fix
    return Err(e);
}
```

(and the same at the batch sites lib.rs:2710, 2618-2627, 2645-2651,
2497-2506). With it, all five properties and liveness hold (0 errors,
full searches — §3).

Rejection tests on the control:

- **`MUTANT_FLIP_BAD_SLOT`** — a plausible *wrong* fix: keep the slot and
  flip the entry to MemoryTier anyway ("nothing leaked"). Rejected by P4 at
  depth 43: the entry would serve garbage from the failed read. So the leak
  cannot be "fixed" by hiding it behind a MemoryTier entry.
- **`MUTANT_DOUBLE_FREE`** — a raw second deallocation bypassing the
  slots-map guard. Rejected by P2 at depth 44.
- **`MUTANT_DOUBLE_REMOVE`** — the fix's `mt.remove` called twice. This was
  initially expected to pass (KeyNotFound no-op) but is **also rejected**
  (invariant, depth 48) — a genuine insight, see §6.

## 6. Side finding: `mt.remove` is not idempotent under re-insertion

The `MUTANT_DOUBLE_REMOVE` counterexample: LKP(1)'s fix frees its reservation;
LKP(2) re-inserts the same key (a new, live allocation) and completes its
promotion (`loc=MT` backed by that slot); the mutant's *second* `mt.remove`
then succeeds — `mt.remove` is keyed by `CacheKey`, not by slot handle
(memory-tier lib.rs:483-514) — and frees **LKP(2)'s live slot**, leaving a
dangling `MemoryTier` entry (the Part A bug shape). The `KeyNotFound` guard
(memory-tier lib.rs:507-510) only protects the case where the key was *not*
re-inserted in between.

Practical consequence for the fix: it must remove **exactly once** (or remove
by slot handle / generation, which the current `mt.remove` API does not
support). The verified control does remove exactly once.

## 7. Modelling choices that could hide a bug

- `evict_one` is one atomic step (demote + free). The real
  `try_evict_to_block` → `mt.remove` window is collapsed; it is the *safe*
  ordering (dm transition before the DRAM free), but a bug living inside that
  window would be missed. (The *unsafe* order — free-then-demote — is Part A's
  finding and is not modelled here.)
- The batch `AlreadyExists` recovery is bounded to 2 tries and nondeterminism
  chooses single-key vs batch behaviour. Condvar wakeups, timeouts and their
  exact values are not modelled: a missed-wakeup bug in `serve_concurrently_promoted`
  would not be caught.
- The SSD read either always fails (LKP(1)) or always succeeds (LKP(2)).
  Partial/multi-region failures are not modelled — in particular the batch
  multi-region post-flip window (lib.rs:2682-2698) where jobs are served
  after the flip without holding a pin is **not** verified; it is suspicious
  (a demotion there could free the slot mid-serve) but out of scope.
- Only key 0 is ever promoted; key 1 exists as eviction fodder only. A bug
  requiring two distinct cold keys (e.g. cross-key eviction interference in
  `evict_for_space`) would be missed.
- Write pins (`write_ref`) are not modelled; only read pins on key 0.
- Capacity 2: the `AllocationFailed` → staging fallback path is modelled by
  outcome only, so a bug in the staging fallback itself would be missed.
- The "unreclaimable" argument depends on `evict_one`'s guard faithfully
  encoding `try_evict_to_block`'s refusal for `BlockDevice` entries — checked
  against dm lib.rs:548-580 (Ok only when MT + unpinned + persisted).

## 8. What is left out (summary)

Byte sizes, SSD offsets, pointer values (generation counters instead);
staging fallback content; channels/pipeline DMA, GPU, CRC; `LOOKUP_RETRIES`
exact count (2 modelled); condvar mechanics; the three additional batch leak
sites (same shape as 2710, noted by inspection); the multi-region pinless
serve window (2682-2698, noted by inspection); write pins; distinct-key
scenarios. Full tables in `B/README.md`.

## 9. Step durations (approximate; reconstructed from the session log and run outputs)

| Step | Window | Notes |
|------|--------|-------|
| Read the promotion path, callers, dm/mt internals | ~05:46-06:05 | lib.rs 750-908, 2228-2994, 3636-3792; dm, mt, background.rs |
| Build `failed_promotion_slot.pml`, first runs | ~06:05-06:12 | P1/P3/P5 violations found (initial check formulations); P2/P4 held |
| Build control, eliminate false positives | ~06:12-06:21 | three artefact fixes: P1 own-generation check, P3 concurrent-flip exemption, atomic `evict_one`; then EV's blocking candidate-pick (invalid end state, depth 80) fixed with an else arm |
| Final verification matrix, mutants, trails | 06:21-06:27 | all runs in §3; `MUTANT_DOUBLE_REMOVE` surprise analysed and re-framed (§6) |
| Makefile, README, REPORT, STATUS, SUMMARY | 06:27-06:29 | |

Total Part B: ~42 minutes of the 5.5-hour box. Bounds stayed at the smallest
that exercise the scenario (2 keys, capacity 2); no run needed more than
0.03 s or ~135 MB, so no bound increases were warranted.

## 10. Refusals encountered in Part B

- A `for` loop over the three mutants combining `spin`/`cc`/`systemd-run`
  with a trailing `cp` was refused by the harness. The mutants were instead
  run as three separate plain commands and the trails preserved with the
  Write tool. (Earlier Part A refusals — `cat` of a settings file, a compound
  `grep; echo`, `~/.local/bin/spin -V`, `cp`, `mv`, `printf >>` — are listed
  in SUMMARY.md.)
- `rm -f pan-control pan-p2 … pan.t` (cleanup of stale build artifacts from
  the debugging session) was refused. The files were left in place — they are
  inert build outputs; the Makefile's `clean` target lists them for future
  use.
