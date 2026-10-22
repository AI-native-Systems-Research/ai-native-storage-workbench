# J22 — a new Spin model from scratch: `clear_memory_tier`

node7, 2026-10-19 (UTC 06:06–06:25). Code: ai-native-storage-certus @ acd24025, read with `git show`; no
repository was edited. Method: workbench `fv/skills/tools-spin-model/SKILL.md`, including its "Added in the
workbench" rules. References read: `~/FV/spin_references/manuals/Manual.html`, `invariance.html` (the
monitor-process pattern used for property (a)), `Pan.html` (-a, -f, NFAIR, MEMLIM, -w), and the tutorial.
Spin 6.5.2. Every pan run used the memory cap (`systemd-run … MemoryMax=60G`, `-DMEMLIM=16000`, `-w24`).

Files are in `~/FV/J22/` (branch `node7/J22-spin-clear-mt`):
- `clear_memory_tier.pml` (396 lines) and `clear_memory_tier_control.pml`;
- `Makefile` and `README.md`, which holds the skill's tables (Properties, System Abstraction, Correspondence,
  Returned values, Left out);
- `*.log` for the four runs, and `trails/` with the trails and their `spin -t -p` replays.

## Headline
**Spin finds the bug on today's code in 0.05 s, in under 300 states. Four of the asked-for and added properties
fail. The control copy with a minimal fix passes all properties, for safety and liveness.**

| run | result | states stored | transitions | depth | pan time | memory | wall / max RSS |
|---|---|---|---|---|---|---|---|
| shipped, `make safety` | **FAIL**: `assertion violated` (PA, first found) | 137 | 151 | 90 | 0 s | 138.9 MB | 2.3 s / 143 MB |
| shipped, `make liveness` | **FAIL**: same assertion (inside the L1 search) | 124 | 430 | 152 | 0 s | 138.9 MB | 2.1 s / 143 MB |
| control, `make safety` | **PASS**, 0 errors | 259,816 | 427,257 | 105 | 0.13 s | 160.5 MB | 2.2 s / 165 MB |
| control, `make liveness` (L1 + acceptance cycles, weak fairness) | **PASS**, 0 errors | 271,534 | 5,024,989 | 215 | 1.67 s | 161.4 MB | 3.8 s / 166 MB |

**Every property on the shipped code**, each run on its own (`-DONLY_<P>`) so that each has its own trail:

| property | shipped | control |
|---|---|---|
| (a) PA: no dispatch-map entry points at a released slot | **FAIL** (127 states) | pass |
| PA1: a lookup never reads a released slot while serving | **FAIL** (10,359 states) | pass |
| PA2: the writer never reads a released slot while writing to SSD | **FAIL** (280 states) | pass |
| (b) PB: after clear_memory_tier, every entry is BlockDevice or gone | **FAIL** (187 states) | pass |
| (c) PC: no pin released by an actor that did not take it | pass (346,315 states, full search) | pass |
| PD: the returned count equals the entries actually cleared | **FAIL** (187 states) | pass |
| PF: final state has no leaked pins and no orphan slot | pass (346,315 states) | pass |
| L1: clear_memory_tier returns | holds on the shipped code with assertions off (302,270 states, weak fairness) | pass |

## The counterexamples in plain words
Both keys start just after a store: each slot is allocated, and each entry is MemoryTier, unpersisted, with one
read pin held by its queued write job (lib.rs:3508 → 3552 → 3561).

**(a) PA: an entry points at a freed slot** (`trails/only_PA.trail`, 125 steps):
1. Two lookups of key 0 each pin, serve from DRAM and release (lib.rs:3011, 3048-3056).
2. The writer persists key 0: `convert_to_storage` sets `ssd_offset` and drops the job's pin (lib.rs:1464;
   dispatch-map 184-195).
3. The writer takes key 1's job and starts writing key 1's slot to SSD (lib.rs:1399, 1451). It still holds
   key 1's pin.
4. `clear_memory_tier` snapshots both keys (lib.rs:3809). For key 0, `try_evict_to_block` succeeds, so the
   entry is now BlockDevice, and the slot is freed (3814-3815). That part is correct.
5. For key 1, `try_evict_to_block` returns **ActiveReferences**, because the writer's pin is held (dispatch-map
   558-560). The shipped code takes the else branch and calls `mt.remove(1)` (lib.rs:3818), **freeing the slot
   while the dispatch map still points at it**. → PA fails.

**PA2** is the same trail one step later. The writer is in the middle of its SSD write of key 1 (lib.rs:1451)
when the slot under it is freed, so it reads freed memory.

**PA1: a lookup reads a freed slot** (`trails/only_PA1.trail`). The writer has persisted both keys. A lookup
pins key 0 and starts its H2D copy from the slot (lib.rs:3011, 3048). `clear_memory_tier` gets ActiveReferences
for key 0 because of the lookup's pin, frees the slot anyway (lib.rs:3818), and its `dm.remove` is refused and
ignored (3819). The lookup's copy completes from the freed slot. → PA1 fails.

**(b) PB and PD: the entry survives and is still counted** (`trails/only_PB.trail`, 135 steps). The first steps
are as in PA (key 0 demoted, slot freed, count 1). Then:
1. Key 1 is pinned by the writer, so `try_evict_to_block` returns ActiveReferences.
2. `mt.remove(1)` frees the slot.
3. `dm.remove(1)` returns **ActiveReferences**, ignored (lib.rs:3819).
4. The writer then finishes and persists key 1. Its `convert_to_storage` sets an `ssd_offset` on an entry whose
   slot is already gone (lib.rs:1464).
5. `count += 1` (3821), and `clear_memory_tier` returns `Ok(2)` (3824).

At return, key 1 is still a **MemoryTier entry in the map**, pointing at a freed slot. So PB fails: it is not
BlockDevice and not gone. PD also fails: 2 is reported but only 1 entry was cleared. This is exactly the
"unpersisted pinned entry stays in the map" case named in the job, and it violates the spec's own description
of the method (dispatcher spec FR-038; interfaces `idispatcher.rs:806-822`: "Entries still being written are
removed entirely").

**Not found:**
- **(c) PC holds.** No actor ever releases another's pin in this model, on the shipped code or the control. The
  J21 "remove releases a foreign pin" defect is in `remove()`, which this model does not include.
- **PF holds.** At quiescence there is no leaked pin and no orphan slot. Freed slots belong to entries that are
  still in the map; that is PA, not an orphan.
- **L1 (termination) holds** on the shipped code.

## The minimal fix (control)
`clear_memory_tier_control.pml` is `#define FIX 1` plus `#include "clear_memory_tier.pml"`. No property, assert,
actor or bound is changed; only `Clear`'s per-key body differs. In Rust terms (lib.rs:3814-3821):

```rust
match dm.try_evict_to_block(key) {
    Ok(()) => { let _ = mt.remove(key); count += 1; }
    Err(DispatchMapError::ActiveReferences(_)) => {}            // pinned: retried by the outer loop
    Err(DispatchMapError::KeyNotFound(_)) => { let _ = mt.remove(key); count += 1; }   // orphan slot
    Err(_) => {                                                 // InvalidState: unpersisted
        if dm.remove(key).is_ok() { let _ = mt.remove(key); count += 1; }   // map entry first
    }
}
```

The outer `loop` (lib.rs:3808) already re-reads `oldest_keys`, so a pinned key is retried once its pin is
released.

Liveness L1 passes on the control under weak fairness. The trade-off is that `clear_memory_tier` now **waits for
pins to drain**. A pin that is never released would make it spin forever. The model cannot show that, because
every actor here terminates; it is a design question for the code owner, not a model result.

## What I modelled, and choices that could hide a bug
**Modelled:**
- Clear, two lookups (warm, cold and NotExist arms) and the writer (success, peek None, and the three failure
  exits) all run concurrently, interleaved by Spin at the granularity of single dispatch-map and memory-tier
  calls. Each call is one atomic step, matching each call's single lock hold.
- 10 returned values are modelled and 6 are listed under Left out, with file:line. Missing: 0.

**Could hide a bug:**
- **Bounds.** There are 2 keys, 2 lookups (one each), 1 job per key and 1 Clear call. A bug that needs a third
  key, repeated lookups, or a second `clear_memory_tier` would not show. N_KEYS=3 is affordable but was not run.
- **The start state is fixed.** It is "both keys stored, both write jobs queued". It does not include entries
  already demoted to BlockDevice before the clear, populate running *during* the clear, or key reuse.
- **No write refs after the start.** `dm.lookup`'s wait for `write_ref == 0` and its `Timeout` are not modelled
  (Left out X2). A populate that holds a write ref during the clear is therefore not explored.
- **Actors left out:** the SSD evictor and the memory-tier background evictor, which also call `mt.remove` (J20
  B2); `remove()`; and the cold-promotion path inside lookup (J20 B3/B5). Each could interact with Clear in ways
  this model cannot see.
- **The writer's three failure exits are merged into one branch.** They act identically on the modelled state,
  but the extent manager (reserve, publish, abort) is not modelled at all (Left out X5).
- **Memory reads are abstract.** A "read of a freed slot" is `mt_present[k] == false` during a window. Slot reuse
  by another key, which is what would make the bug visible as corrupt data, is not modelled.
- **Unreached code.** In the control, 6 of 105 Clear states and 7 of 80 BgWriter states are unreached. They are
  error results that cannot occur in this configuration: `mt.remove` on an absent slot, `convert_to_storage` on
  an absent or BlockDevice entry, and peek `None`. The listing is in `clear_memory_tier_control.safety.log`.

## Time per step (UTC, 2026-10-19)
| step | from → to | time |
|---|---|---|
| read the job, the references and the code (dispatcher, dispatch-map, memory-tier, spec FR-038, interface doc) | 06:06 → 06:10 | ~4 min |
| write the model, Makefile and first run | 06:10 → 06:14 | ~4 min |
| classify the failures: one run per property, replace the ad-hoc assert toggling by `CHK_<P>` macros | 06:14 → 06:17 | ~3 min |
| control: fix two setup errors of mine (a nested `/* */` in a comment crashed the Spin lexer; liveness needed `-DNFAIR=3`), final runs, trails | 06:17 → 06:20 | ~3 min |
| re-derive and correct the line citations (5 were off by 2-3 lines), README tables, this report | 06:20 → 06:25 | ~5 min |

The total is about 20 minutes of the 3-hour box. Most of the reading of these three components had already been
done in J18-J21, which is why the first step was fast.

**One run produced a flood of files.** It was an exploratory `-e` run (save every error's trail), and it wrote
about 72k trail files to a scratch directory. I deleted them and switched to one targeted run per property.
