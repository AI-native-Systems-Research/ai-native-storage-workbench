# clear_memory_tier vs a pinned lookup vs the background writer

**Scope**: Whole system. The actors are the dispatcher's `clear_memory_tier`, two concurrent `lookup_async`
callers, and the background write-through worker. The model also includes the dispatch map and memory tier
those three share.

This model was written from scratch for node7 J22 (2026-10-18), using ai-native-storage-certus @ acd24025. The
method is the workbench `tools-spin-model` skill, including its "Added in the workbench" rules. Every value the
modelled API returns is either modelled (see the Returned values table) or listed under Left out, and every
file:line was re-derived from the code at acd24025.

The model checks whether `clear_memory_tier` (dispatcher lib.rs:3794-3825) is safe when an entry is pinned
while it clears. A pin is either a lookup's read reference or the write job's downgraded reference.

The concern is that the shipped code ignores the dispatch map's refusal. For a **persisted** pinned entry,
`try_evict_to_block` refuses with `ActiveReferences`, the else branch frees the slot (`mt.remove`), and
`dm.remove` then also refuses, again ignored. The entry stays MemoryTier and points at the freed slot. For an
**unpersisted** pinned entry, the same path leaves the entry in the map.

## Properties Verified

| ID | Property | Type | Where |
|---|---|---|---|
| PA | (a) No dispatch-map MemoryTier entry points at a released memory-tier slot | Safety (invariant, every state) | `Monitor` |
| PA1 | A lookup never reads a released slot during its H2D serve window | Safety | `Looker` |
| PA2 | The writer never reads a released slot while writing it to SSD | Safety | `BgWriter` |
| PB | (b) When `clear_memory_tier` returns, every entry is BlockDevice (with its SSD offset) or gone | Safety | `Clear`, at return |
| PC | (c) No pin is released by an actor that did not take it (ghost owner count per actor) | Safety | `release_read`, `convert_to_storage` |
| PD | The count `clear_memory_tier` returns equals the number of entries it actually cleared | Safety | `Clear`, at return |
| PF | Final state: no leaked pins, no orphan memory-tier slot | Safety | `init`, at quiescence |
| L1 | `clear_memory_tier` eventually returns (`<> clear_done`) | Liveness (LTL, weak fairness) | `make liveness` |

## System Abstraction

| Real component / thread | Promela |
|---|---|
| `IDispatcher::clear_memory_tier` (caller thread) | `proctype Clear` |
| `IDispatcher::lookup_async` (two client threads) | `proctype Looker(1)`, `Looker(2)`, each doing one lookup of a nondeterministic key |
| `BackgroundWriter::worker_loop` → `process_write_job` | `proctype BgWriter`, one job per key from `chan wq` |
| `DispatchMap` (one `Mutex<Inner>`) | globals `dm_present/dm_loc/dm_has_off/read_ref`; every call is one `atomic` step |
| `MemoryTier` pool (`RwLock` on the pool) | global `mt_present[]`; `oldest_keys`, `remove` and `peek` are each one atomic step |
| Pin ownership (no runtime counterpart) | ghost `own[actor][key]`, used only by PC and PF |
| Invariant PA | `Monitor`, which blocks until PA is false (Spin invariance pattern, manuals/invariance.html) |

**Start state.** It is the state right after `copy_gpu_to_memory_completed` for both keys:
1. `create_memory_tier_entry` (lib.rs:3508) gives `write_ref = 1`;
2. `downgrade_reference` (lib.rs:3552) turns that into `read_ref = 1`, owned by the write job;
3. `writer.enqueue` (lib.rs:3561) queues the job.

Both slots are allocated, and both entries are MemoryTier with no SSD offset.

## Assumptions / Stubs
- Each dispatch-map call is one atomic step. This matches the code, where each takes `inner.lock()` for its
  whole body. Each memory-tier call is also atomic (pool lock).
- `dm.lookup`'s wait for `write_ref == 0` is not modelled. No write ref exists after the start state.
- The writer's three failure exits (DmaBuffer, `reserve_extent`, SSD write) are one nondeterministic branch,
  because all three do the same thing to the modelled state: `release_read` at lib.rs:1432/1442/1454.
- `mt.oldest_keys(64)` returns every resident key. With 2 keys, 64 covers all of them. The model takes it as one
  snapshot, as the code does.
- The H2D copy and the SSD write are modelled as a window: a `skip` between two `PA1`/`PA2` asserts.

## Running
```bash
export PATH=$HOME/.local/bin:$PATH
make clean && make safety           # assertions + invalid end states (memory-capped: systemd-run + MEMLIM + -w24)
make liveness                       # LTL L1 under weak fairness (-a -f), NFAIR=3
make safety   MODEL=clear_memory_tier_control.pml
make liveness MODEL=clear_memory_tier_control.pml
# one counterexample per property:
spin -a -DONLY_PA1 clear_memory_tier.pml && cc -O2 -DMEMLIM=16000 -DSAFETY -o pan pan.c && ./pan -w24
spin -t -p -DONLY_PA1 clear_memory_tier.pml        # replay
```
Note that `make clean` deletes `*.trail`. Kept copies are in `trails/`.

## Tuning the Model
- `N_KEYS=2` and `N_LOOK=2` give 3 concurrent actors plus Clear.
- `N_LOOK` above 2 needs a larger `NFAIR` for liveness.
- The control explores about 260k states, 160 MB and 2 s, so `N_KEYS=3` is affordable if needed. It was not
  run (see the J22 report).

## Correspondence to Source Code (acd24025; re-derived by symbol)

| Model location | Source | Lines |
|---|---|---|
| `Clear`: candidate snapshot | `components/dispatcher/src/lib.rs` `mt.oldest_keys(64)` | 3809 |
| `Clear`: empty → break | lib.rs | 3810-3812 |
| `Clear`: `dm_try_evict` | lib.rs `dm.try_evict_to_block(key).is_ok()` | 3814 |
| `Clear`: OK → `mt_remove` | lib.rs `let _ = mt.remove(key)` | 3815 |
| `Clear`: else → `mt_remove`, `dm_remove` (results ignored) | lib.rs `let _ = mt.remove(key); let _ = dm.remove(key)` | 3818-3819 |
| `Clear`: `clear_count++`, return | lib.rs `count += 1`, `Ok(count)` | 3821, 3824 |
| `Looker`: `dm_lookup` | lib.rs `dm.lookup(key)` | 3011 |
| `Looker`: NotExist | lib.rs | 3029 |
| `Looker`: MemoryTier serve, release | lib.rs `serve_memory_tier_to_gpu`, `dm.release_read` | 3048-3055, 3056 |
| `Looker`: BlockDevice release | lib.rs `dm.release_read` | 3064 |
| `BgWriter`: recv | `components/dispatcher/src/background.rs` `recv_timeout` | 126 |
| `BgWriter`: `mt_peek`; None → release | lib.rs | 1399; 1402 |
| `BgWriter`: failure exits → release | lib.rs | 1432, 1442, 1454 |
| `BgWriter`: SSD write window | lib.rs `write_buffer_to_ssd` | 1451 |
| `BgWriter`: success → `dm_convert_to_storage` | lib.rs | 1464 |
| `dm_lookup` | `components/dispatch-map/src/lib.rs` `lookup` (NotExist 143, pin 147-149, location 157-158) | 128-174 |
| `dm_release_read` | dispatch-map lib.rs `release_read` (KNF 272, underflow 275, decrement 278) | 267-285 |
| `dm_convert_to_storage` | dispatch-map lib.rs (KNF 181, set offset 184-185, BlockDevice → InvalidState 187-188, drop one read ref 194-195) | 176-208 |
| `dm_try_evict` | dispatch-map lib.rs `try_evict_to_block` (KNF, ActiveReferences, demote, InvalidState) | 548-580 |
| `dm_remove` | dispatch-map lib.rs `remove` (KNF 339, ActiveReferences 342, remove 346) | 334-357 |
| `mt_remove`, `mt_peek` | `components/memory-tier/src/lib.rs` `remove`, `peek` | 483, 423-433 |
| start state | dispatcher lib.rs `create_memory_tier_entry`, `downgrade_reference`, `writer.enqueue` | 3508, 3552, 3561 |
| Spec | dispatcher spec 001 FR-038 (l.315); dispatch-map spec FR-011 (l.252), FR-026 (l.267); interfaces `idispatcher.rs` doc for `clear_memory_tier` (l.806-822) | |

## Returned values (J19 rule)

| # | Value | Produced at | In the model |
|---|---|---|---|
| M1 | `dm.lookup` → `Ok(NotExist)` / `Ok(MemoryTier)` / `Ok(BlockDevice)` | dispatch-map 143, 157-158 | `R_NOTEXIST` / `R_MT` / `R_BD`, each with its own Looker branch |
| M2 | `dm.release_read` → `Ok` / `KeyNotFound` / `RefCountUnderflow` | dispatch-map 272-278 | `OK` / `E_KNF` / `E_UNDERFLOW` (ignored by callers, as in the code: `let _`) |
| M3 | `dm.convert_to_storage` → `Ok` / `KeyNotFound` / `InvalidState` | dispatch-map 181-191 | `OK` / `E_KNF` / `E_INVALID` |
| M4 | `dm.try_evict_to_block` → `Ok` / `KeyNotFound` / `ActiveReferences` / `InvalidState` | dispatch-map 548-580 | `OK` / `E_KNF` / `E_ACTIVE` / `E_INVALID`; the shipped code branches only on `is_ok()`, the control on all four |
| M5 | `dm.remove` → `Ok` / `KeyNotFound` / `ActiveReferences` | dispatch-map 339-346 | `OK` / `E_KNF` / `E_ACTIVE` (ignored in shipped code: PB/PD show the effect) |
| M6 | `mt.remove` → `Ok` / `Err` | memory-tier 483 | `OK` / `E_KNF` (ignored, as in the code) |
| M7 | `mt.peek` → `Some` / `None` | memory-tier 423-433 | `R_SOME` / `R_NONE` |
| M8 | `mt.oldest_keys` → the candidate list | memory-tier 435 | `cand[]` snapshot |
| M9 | `clear_memory_tier` → `Ok(count)` | dispatcher 3824 | `clear_count`, checked by PD |
| M10 | `lookup_async` → `Err(KeyNotFound)` / `Ok(stream)` (warm) / cold result | dispatcher 3029, 3061, 3065-3067 | `look_ret[a] = L_KNF` / `L_OK` / `L_COLD` |

**Summary row:** 10 modelled / 6 left out / 0 missing.

## Left out

| # | What | file:line | What the model therefore cannot see |
|---|---|---|---|
| X1 | `ensure_initialized` and the receptacle `get()` errors (`NotInitialized`) | dispatcher 3795-3805 | `clear_memory_tier` / `lookup_async` failing early; neither touches the state the properties concern |
| X2 | `dm.lookup` `Err(Timeout)` (writer holding `write_ref` past 2 s) and `RefCountOverflow` | dispatch-map 136-150 | a lookup that takes no pin; the model has no write refs after the start state |
| X3 | `promote_and_serve` (cold path, BlockDevice arm) and its results | dispatcher 3065 | the cold-promotion interaction with Clear (J20 B3/B5 cover it) |
| X4 | `serve_memory_tier_to_gpu` result (`r`) and the `MismatchSize` / integrity-CRC arms | dispatcher 3030-3046, 3048-3060 | GPU-copy failures; they change no modelled state |
| X5 | `write_handle.publish()` result and the extent manager | dispatcher 1463 | extent leaks (J20 B1/H1); PB looks only at the dispatch map |
| X6 | the no-drives synthetic-offset branch of `process_write_job` | dispatcher 1406-1411 | it behaves as a successful write for this model's purposes; not split out |

**Not modelled at all:** the SSD evictor and the memory-tier background evictor, which also call `mt.remove`
(J20 B2); `remove()`; `populate` of new keys during the clear; key reuse; and more than one call to
`clear_memory_tier`.
