# clear_memory_tier: pinned-slot safety

## Scope

Single component (dispatcher), three concurrent actors:
`clear_memory_tier` itself, a client lookup holding a dispatch-map read pin,
and the background write-through writer.

The model verifies the interaction of `DispatcherComponent::clear_memory_tier`
(`code/components/dispatcher/src/lib.rs:3794-3825`) with concurrent readers and
the write-through writer. Two builds exist:

- `clear_memory_tier.pml` — faithful model of the **current code**.
- `clear_memory_tier_control.pml` — **control copy** with a minimal fix in the
  force-remove path, plus a known-bad mutant arm (`-DMUTANT_RELEASE_PINS`).

## Properties Verified

| ID | Property | Type |
|----|----------|------|
| (a) | No dispatch-map entry points at a released memory slot (global invariant `loc==MT ⟹ slot`, plus use-side checks: a pinned reader/writer's slot must be live and unchanged) | Safety |
| (b) | After `clear_memory_tier` returns, every entry is BlockDevice with an SSD offset or is gone (an MT entry with a *live* slot is exempt: it can only be a store that began after the tier was drained, since clear returns only when `mt.oldest_keys` is empty) | Safety |
| (c) | No pin is released by an actor that did not take it (asserted at every `release_read` / `convert_to_storage` call site) | Safety |
| (L) | Once both bounded actors finish, `clear_memory_tier` eventually returns | Liveness |

## System Abstraction

| Real component | Promela process |
|----------------|-----------------|
| `clear_memory_tier` call (lib.rs:3794-3825) | `CLR` |
| Client `lookup_async` MemoryTier hit (lib.rs:3036-3062) | `LKP` (bounded to 2 lookups) |
| Store path `populate` → write-through job → `process_write_job` (lib.rs:3143-3207, 3489-3568, 1390-1465; background.rs:25-143) | `WTR` (bounded to 2 store cycles) |
| DispatchMap (dispatch-map/src/lib.rs) | shared arrays `loc`, `persisted`, `rr` + atomic inlines |
| MemoryTier pool (memory-tier/src/lib.rs:70-115, 328-514) | shared arrays `slot`, `gen` + atomic inlines |

## Returned values (modelled ⟷ code)

| Operation | Modelled returns | Not modelled (Left out) |
|-----------|------------------|-------------------------|
| `dm.try_evict_to_block` (dm lib.rs:548-580) | Ok(demote) / Err (all three variants collapsed to one `else`) | which error variant reached the (unused) result |
| `dm.remove` (dm lib.rs:334-357) | Ok / Err(ActiveReferences) | KeyNotFound (unreachable for a candidate) |
| `mt.remove` (mt lib.rs:483-514) | Ok(free) / Err(KeyNotFound, no-op) | NotInitialized |
| `dm.lookup` (dm lib.rs:128-174) | NotExist / MemoryTier{ptr,gen} / BlockDevice (cold path out of scope) | Timeout, RefCountOverflow, MismatchSize, exact pointer/size values |
| `dm.release_read` (dm lib.rs:267-285) | Ok / (RefCountUnderflow forbidden by property (c) assert) | KeyNotFound |
| `dm.convert_to_storage` (dm lib.rs:176-208) | Ok(persist+unpin) / InvalidState (pin leak, unreachable in-model) | — |
| `mt.insert` (mt lib.rs:328-390) | Ok / AlreadyExists-decline | PoolFull, InvalidSize (capacity not modelled) |
| `clear_memory_tier` (lib.rs:3794-3825) | Ok | count value, NotInitialized |

## Left out

- Byte sizes, SSD offsets (a BlockDevice entry always implicitly carries its
  retained offset), pointer values (replaced by generation counters).
- Memory-tier capacity and the populate-path eviction
  (`evict_for_space`/`evict_one_clean`, lib.rs:1050-1168) — the pool never
  fills in this model; only `clear_memory_tier` frees slots.
- The `create_memory_tier_entry`(wr=1) → `downgrade_reference`(wr→rr) window
  (lib.rs:3508-3552) is collapsed into one atomic step; `write_ref` is never
  observable. A lookup that arrives in that window blocks in reality
  (`wait_for`, dm lib.rs:131-139) rather than racing.
- The cold (BlockDevice) serve path of lookup (`promote_and_serve`,
  lib.rs:3063-3065) — Part B covers it.
- `BackgroundEvictor` / `MemoryTierEvictor` (background.rs:232-575) — other
  slot freers; their `mt.remove`-after-`dm` order is the safe order already.
- CRC/integrity checks, channels/pipeline DMA mechanics (reduced to
  use-asserts), `LOOKUP_RETRIES`, staging buffers, GPU.
- The return count of `clear_memory_tier`.
- 2 keys, 1 lookup actor, 1 writer actor, ≤2 pins per key.

## Specification basis

There is no spec clause for `clear_memory_tier` in the snapshot (the skill's
spec paths under `components/dispatcher/specs/` are not present in this
read-only snapshot). The asserted properties therefore come from the task
statement and from the code's own stated invariants:

- (a)/(c) rest on the pin contract documented in `dispatcher/src/pins.rs:8-21`
  ("that refusal is the only thing stopping … handing its DRAM slot back to
  the pool while a copy is still reading it") and `evict_one_clean`'s contract
  (lib.rs:1041-1046: "this NEVER frees a slot that a pinned, in-flight load
  still points at"). `clear_memory_tier` is held to the same standard.
- (b) is the task statement's postcondition for the admin operation.

**Spec gap (finding):** `clear_memory_tier`'s else-branch comment (lib.rs:3817)
assumes the only `try_evict_to_block` failure is "no SSD copy". The dispatch
map has a second refusal — `ActiveReferences` (dm lib.rs:558-560), explicitly
documented as "a held pin is its own variant" — for which the force-remove is
unsafe. The code's own doc for `evict_one_clean` (lib.rs:1041-1046) states the
safety rule that `clear_memory_tier` violates.

## Correspondence

| Model location | Intended behavior (spec / documented contract) | Implemented behavior (source) |
|---|---|---|
| `CLR` loop guard `(slot[0]\|\|slot[1])` / `else break` | drain until `oldest_keys` empty | lib.rs:3808-3812 (`mt.oldest_keys(64)`) |
| `dm_try_evict_to_block` | demote only unpinned, persisted entries | dm lib.rs:548-580 (guard 558, demote 562-569) |
| `CLR` evict_ok branch: `mt_remove` after demote | dm transition before DRAM free | lib.rs:3814-3815 |
| `CLR` else branch: `mt_remove` THEN `dm_remove` | **should be**: dm first, skip pinned (evict_one_clean contract, lib.rs:1041-1046) | lib.rs:3816-3820: `mt.remove` frees the slot first, `dm.remove` fails on ActiveReferences (**divergence — the finding**) |
| `mt_remove` no pin check | pins live only in dispatch-map; mt cannot see them | mt lib.rs:483-514 (remove+deallocate 507-513) |
| `LKP` `dm_lookup` → `use_slot` → `dm_release_read` | pin protects the slot for the DMA | lib.rs:3011, 3036-3062; dm lib.rs:128-174 |
| `WTR` store prologue (atomic) | reserve slot, create entry, downgrade, enqueue | lib.rs:3506-3566; dm lib.rs:393-439, 307-332 |
| `WTR` peek → use → convert/release | write-through releases its pin via `convert_to_storage` | lib.rs:1399-1464; dm lib.rs:176-208 (rr decrement 194-196) |

## Mutant

- Known-bad build #1 = the **original** model (`make orig`): expected and
  observed violations of (a) and (b) (see REPORT.md).
- Known-bad build #2 = `make mutant` (`-DMUTANT_RELEASE_PINS` on the control):
  a plausible wrong fix that force-releases outstanding pins so `dm.remove`
  succeeds. Expected and observed: `assert(clrmypin)` violation (property (c)
  rejects it).
- Fixed build (`make control`): 0 errors.

## Running

```sh
make control      # fixed build: 0 errors
make orig         # current code: (a)+(b) violated, trails written
make prop-a prop-b prop-c   # per-property attribution on the original
make mutant       # control + pin-releasing wrong fix: (c) violated
make liveness     # control, acceptance cycles: 0 errors
# trail replay (same -D flags as the generating run):
spin -t -p -g -l clear_memory_tier.pml
spin -t -p -g -l -DMUTANT_RELEASE_PINS clear_memory_tier_control.pml
```

Every `pan` run goes through:
`systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p OOMPolicy=continue timeout 1800 ./pan… -m100000`
and `pan` is compiled with `-DMEMLIM=16000`.

## Tuning the Model

- `NKEYS` (2): keys in the pool. Raising to 3 grows the state space
  multiplicatively; the counterexamples need only 1 key.
- LKP/WTR iteration bound (2): raise for deeper interleavings; 2 suffices to
  reach every hazard here.
- `gen` is a byte and never wraps in these bounds (max 3).
