/*
 * Component-level model: failed cold promotion — CONTROL COPY (minimal fix)
 * ========================================================================
 *
 * Identical to failed_promotion_slot.pml except for the marked fix in the
 * failed-read branch of LKP. The fix mirrors what promote_to_memory_tier
 * already does (dispatcher lib.rs:3770-3771): release the reserved
 * memory-tier slot before returning the error.
 *
 *   original (lib.rs:882-884):          fix:
 *     if let Some(Err(e)) = ... {           if let Some(Err(e)) = ... {
 *         return Err(e);                        let _ = mt.remove(key);
 *     }                                          return Err(e);
 *                                             }
 *
 * (The same fix is needed at the batch leak sites: lib.rs:2710, 2618-2627,
 * 2645-2651, 2497-2506.)
 *
 * Expected: all five properties hold (0 errors), safety and liveness.
 *
 * Mutants (rejection tests, see Makefile):
 *   -DMUTANT_FLIP_BAD_SLOT  — a plausible WRONG fix: keep the slot and flip
 *     the entry to MemoryTier anyway (nothing "leaked"), serving garbage.
 *     Property P4 must reject it.
 *   -DMUTANT_DOUBLE_REMOVE  — calls the fix's mt.remove twice. ALSO REJECTED
 *     (by the MT-entry-points-at-live-slot invariant): mt.remove is keyed by
 *     CacheKey, not by slot handle (memory-tier lib.rs:483-514), so it is NOT
 *     idempotent under concurrent re-insertion — if another lookup re-inserts
 *     the key between the two removes, the second remove frees THAT live
 *     slot, leaving a dangling MemoryTier entry (the Part A bug shape). The
 *     KeyNotFound guard (memory-tier lib.rs:507-510) only protects the case
 *     where the key was not re-inserted. Lesson: the fix must remove exactly
 *     once (or remove by slot handle / generation).
 *   -DMUTANT_DOUBLE_FREE    — a raw second deallocation that bypasses the
 *     slots-map guard. Property P2 must reject it.
 *
 * Run (see Makefile):
 *   spin -a failed_promotion_slot_control.pml
 *   cc -O2 -DMEMLIM=16000 -DSAFETY -o pan pan.c
 *   systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 \
 *     -p OOMPolicy=continue timeout 1800 ./pan -m100000
 */

#define NKEYS 2
#define CAP 2                       /* memory-tier capacity in slots */
#define USED (slot[0] + slot[1])
#define orphan ((slot[0] && loc[0] != MT) || (slot[1] && loc[1] != MT))

mtype = { ABSENT, MT, BD };
mtype = { RES_OK, RES_ALREADY, RES_ALLOC };

/* ---- dispatch-map entry state (dispatch-map/src/entry.rs, lib.rs) ---- */
mtype loc[NKEYS];      /* Location: ABSENT = no entry, MT = MemoryTier, BD = BlockDevice */
bool persisted[NKEYS]; /* MemoryTier{ ssd_offset: Some(_) }: demotable */
byte rr[NKEYS];        /* DispatchEntry::read_ref (dispatch-map lib.rs:147) */

/* ---- memory-tier pool state (memory-tier lib.rs:70-115, 328-390) ---- */
bool slot[NKEYS];      /* pool.slots contains key */
byte gen[NKEYS];       /* slot generation: bumped on every free (ABA detector) */
bool slot_bad[NKEYS];  /* slot content came from a FAILED SSD read (garbage) */

/* ---- outcomes / observability ---- */
bool l1_done, l2_done, l1_ok, l2_ok;
bool l2_started_after;  /* LKP(2) began after LKP(1) had already returned */

/* Per-family assertion switches (all on by default) */
#ifdef NO_P1
#define P1_CHECK(cond) skip
#else
#define P1_CHECK(cond) assert(cond)
#endif
#ifdef NO_P2
#define P2_CHECK(cond) skip
#else
#define P2_CHECK(cond) assert(cond)
#endif
#ifdef NO_P3
#define P3_CHECK(cond) skip
#else
#define P3_CHECK(cond) assert(cond)
#endif
#ifdef NO_P4
#define P4_CHECK(cond) skip
#else
#define P4_CHECK(cond) assert(cond)
#endif
#ifdef NO_P5
#define P5_CHECK(cond) skip
#else
#define P5_CHECK(cond) assert(cond)
#endif

/* Invariant (as in Part A): a MemoryTier dispatch-map entry always points at
 * a live memory-tier slot. */
#define INV(k) assert(loc[k] != MT || slot[k])

/* dm.lookup — dispatch-map lib.rs:128-174. Takes a read pin (147). */
inline dm_lookup(k, hit, mypin, snap) {
    atomic {
        if
        :: loc[k] != ABSENT ->
            rr[k] = rr[k] + 1;
            mypin = true;
            hit = (loc[k] == MT);
            snap = gen[k]
        :: else ->
            hit = false;
            mypin = false
        fi;
        INV(k)
    }
}

/* dm.release_read — dispatch-map lib.rs:267-285. */
inline dm_release_read(k, mypin) {
    atomic {
        assert(mypin);
        rr[k] = rr[k] - 1;
        mypin = false;
        INV(k)
    }
}

/* mt.remove — memory-tier lib.rs:483-514. The slots-map remove (507-510)
 * returns the slot ONCE; a second remove gets KeyNotFound and deallocates
 * nothing — this guard IS property P2's structural guarantee. */
inline mt_remove(k) {
    atomic {
        if
        :: slot[k] ->
            slot[k] = false;
            gen[k] = gen[k] + 1;
            slot_bad[k] = false
        :: else ->
            skip                    /* KeyNotFound (memory-tier lib.rs:510) */
        fi;
        INV(k)
    }
}

/* dm.promote_block_to_memory_tier — dispatch-map lib.rs:482-528.
 * In-place BlockDevice → MemoryTier flip; keeps the SSD offset
 * (ssd_offset: Some(offset), lib.rs:503-508) and all refs. Only ever called
 * AFTER a successful SSD read (lib.rs:903, 2681-2691). P4: never flip a slot
 * whose read failed. */
inline dm_promote_flip(k) {
    atomic {
        P4_CHECK(!slot_bad[k]);
        if
        :: (loc[k] == BD) ->
            loc[k] = MT;
            persisted[k] = true     /* SSD offset retained (lib.rs:507) */
        :: else ->
            skip                    /* InvalidState — unreachable in-model */
        fi;
        INV(k)
    }
}

/* One eviction candidate: dm.try_evict_to_block (dispatch-map lib.rs:548-580,
 * Ok only when MT + unpinned + persisted) then mt.remove — the discipline of
 * evict_one_clean (lib.rs:1066-1071) and MemoryTierEvictor
 * (background.rs:531-537). The demote→free pair is one atomic step here:
 * it is the SAFE ordering (dm transition before the DRAM free) and no Part B
 * property depends on the (benign) window between the two real locks. */
inline evict_one(k, freed) {
    atomic {
        if
        :: (loc[k] == MT && rr[k] == 0 && persisted[k]) ->
            loc[k] = BD;            /* demote; SSD offset retained */
            if
            :: slot[k] ->
                slot[k] = false;
                gen[k] = gen[k] + 1;
                slot_bad[k] = false
            :: else ->
                skip                /* KeyNotFound: someone else freed it */
            fi;
            freed = true
        :: else ->
            freed = false           /* pinned, or entry not MT (e.g. a leaked
                                       slot of a failed promotion: BD entry) */
        fi;
        INV(k)
    }
}

/* evict_one_clean's scan over the oldest candidates (lib.rs:1056-1102):
 * tries candidates until one is freed. Two keys → try both. */
inline evict_scan(freed_any) {
    bool f;
    freed_any = false;
    if
    :: slot[0] -> evict_one(0, f); freed_any = f
    :: else -> skip
    fi;
    if
    :: (!freed_any && slot[1]) -> evict_one(1, f); freed_any = f
    :: else -> skip
    fi
}

/* evict_and_insert — lib.rs:1176-1224: evict_for_space (1123-1168) then
 * mt.insert (1188; AlreadyExists checked before capacity, memory-tier
 * lib.rs:356-364). */
inline evict_and_insert(k, res) {
    bool freed_any;
    res = RES_OK;
    do
    :: (res == RES_OK && USED + 1 > CAP) ->
        evict_scan(freed_any);
        if
        :: freed_any -> skip        /* re-check capacity (loop) */
        :: else ->
            res = RES_ALLOC         /* AllocationFailed (lib.rs:1149,1161) */
        fi
    :: else -> break
    od;
    if
    :: (res == RES_OK) ->
        atomic {
            if
            :: slot[k] ->
                res = RES_ALREADY   /* AlreadyExists (memory-tier lib.rs:356-358;
                                      dispatcher lib.rs:1208-1217) */
            :: else ->
                slot[k] = true;     /* mt.insert: allocate + track (375-386) */
                gen[k] = gen[k] + 1;
                slot_bad[k] = false
            fi;
            INV(k)
        }
    :: else -> skip
    fi
}

/*
 * The cold lookup. id = 1: SSD read fails (the scenario under test).
 * id = 2: healthy SSD. Both target key 0, the cold (BlockDevice) key.
 */
proctype LKP(byte id) {
    byte k = 0;
    bool mypin = false;
    bool hit = false;
    byte snap = 0;
    mtype res;
    bool outcome_ok = false;
    byte tries = 0;

    if
    :: id == 2 -> atomic { l2_started_after = l1_done }
    :: else -> skip
    fi;

    /* Classify: dm.lookup → BlockDevice hit → pin released BEFORE promoting
     * (lib.rs:3063-3065 single-key; lib.rs:2428-2429 batch). */
    dm_lookup(k, hit, mypin, snap);
    if
    :: mypin -> dm_release_read(k, mypin)
    :: else -> skip
    fi;

    /* Cold promotion: reserve a slot (lib.rs:767-790 / 2557-2558). */
    evict_and_insert(k, res);
    if
    :: (res == RES_OK) ->
        atomic { snap = gen[k] };
        /* Pipelined SSD read into the reserved slot (lib.rs:870-884 single,
         * 2664-2673 batch). */
        if
        :: (id == 1) ->
            /* This lookup's SSD read fails. */
            atomic { slot_bad[k] = true };   /* the slot holds garbage */
#ifdef MUTANT_FLIP_BAD_SLOT
            /* Known-bad WRONG fix (P4 rejection test): keep the slot and
             * flip the entry anyway — nothing "leaked", but the entry now
             * serves garbage from the failed read. */
            dm_promote_flip(k);
            outcome_ok = true
#else
            /* ---- CONTROL FIX (mirrors lib.rs:3770-3771): release the
             * reserved slot before returning the error. ---- */
            mt_remove(k);
#ifdef MUTANT_DOUBLE_FREE
            /* Known-bad (P2 rejection test): a raw second deallocation that
             * bypasses the slots-map guard. */
            atomic {
                P2_CHECK(slot[k]);
                slot[k] = false;
                gen[k] = gen[k] + 1;
                slot_bad[k] = false
            }
#endif
#ifdef MUTANT_DOUBLE_REMOVE
            /* Rejected by the invariant: a second mt.remove is only a no-op
             * if the key was NOT re-inserted in between; mt.remove is keyed
             * by CacheKey (memory-tier lib.rs:483-514), so with a concurrent
             * re-insert it frees the NEW live slot (dangling MT entry). */
            mt_remove(k);
#endif
            /* P1: THIS lookup's own reservation (generation `snap`) must be
             * released — a concurrent re-insert of the same key is a
             * different, live allocation and is not a leak. */
            P1_CHECK(!slot[k] || gen[k] != snap);    /* P1 */
            /* P3(i): the failed promotion left the entry correct — still
             * BlockDevice, or MemoryTier backed by a DIFFERENT live slot
             * (gen != snap), i.e. a concurrent lookup's successful promotion. */
            P3_CHECK(loc[k] == BD || (loc[k] == MT && gen[k] != snap));
            outcome_ok = false               /* lookup returns Err */
#endif
        :: (id == 2) ->
            /* Healthy read: slot filled, single-region H2D served in-pipeline,
             * then the in-place flip (lib.rs:903 / 2681-2691). */
            dm_promote_flip(k);
            outcome_ok = true
        fi
    :: (res == RES_ALREADY) ->
        /* Lost the mt.insert race (lib.rs:1208-1217). */
        if
        :: /* single-key path: promote_and_serve has NO recovery here —
             * `Err(e) => return Err(e)` (lib.rs:789). */
            outcome_ok = false
        :: /* batch path: recovery pass (lib.rs:2937-2946) →
             * serve_concurrently_promoted (lib.rs:1349-1388): bounded wait for
             * the winner's MemoryTier transition, else Err(KeyNotFound). */
            tries = 0;
            do
            :: (tries < 2 && loc[k] != MT) ->
                tries = tries + 1;
                dm_lookup(k, hit, mypin, snap);       /* lib.rs:1360 */
                if
                :: hit ->
                    /* Winner flipped: serve warm under the pin (1361-1366). */
                    P4_CHECK(!slot_bad[k]);           /* P4 */
                    assert(slot[k] && gen[k] == snap); /* pin keeps the slot alive */
                    dm_release_read(k, mypin);
                    outcome_ok = true;
                    break
                :: mypin ->
                    dm_release_read(k, mypin)          /* BlockDevice: back off
                                                          (1368-1375) */
                :: else ->
                    skip                              /* NotExist: no pin (1384) */
                fi
            :: else ->
                outcome_ok = false;                    /* timeout → Err(KeyNotFound)
                                                          (1372-1373) */
                break
            od
        fi
    :: (res == RES_ALLOC) ->
        /* AllocationFailed → staging fallback: served uncached, Ok
         * (lib.rs:770-788 / 2576-2578 → serve_cold_staged 919-1030). */
        outcome_ok = true
    fi;

    atomic {
        if
        :: id == 1 -> l1_ok = outcome_ok; l1_done = true
        :: id == 2 -> l2_ok = outcome_ok; l2_done = true
        fi
    }
}

/*
 * The eviction path that frees memory: MemoryTierEvictor round
 * (background.rs:526-554) — same try_evict_to_block → mt.remove discipline
 * as evict_one_clean. Unbounded: it keeps scanning while any slot exists.
 */
proctype EV() {
    bool freed;
    do
    :: (slot[0] || slot[1]) ->
        if
        :: slot[0] -> evict_one(0, freed)
        :: slot[1] -> evict_one(1, freed)
        :: else -> skip    /* candidate vanished between scan and pick:
                              the real loop just moves to the next one
                              (error returns, background.rs:531-537) */
        fi
    :: else -> break
    od
}

/* Final checks once both lookups have returned. */
proctype FIN() {
    do
    :: (l1_done && l2_done) -> break
    od;
#ifndef LIVENESS
    atomic {
        /* P3(ii): a lookup that started AFTER the failed promotion returned
         * must be able to read the entry again. */
        P3_CHECK(!(l2_started_after && !l2_ok));
        /* P5: no pin leaked … */
        P5_CHECK(rr[0] == 0 && rr[1] == 0);
        /* … and no orphaned (unreclaimable) slot: a live slot must belong to
         * a MemoryTier dispatch-map entry. */
        P5_CHECK(!orphan)
    }
#endif
}

init {
    /* Key 0: cold — BlockDevice entry (on SSD), no memory-tier slot.
     * Key 1: warm — MemoryTier, persisted (write-through landed), unpinned. */
    loc[0] = BD;  slot[0] = false; persisted[0] = true;
    loc[1] = MT;  slot[1] = true;  persisted[1] = true;
    rr[0] = 0; rr[1] = 0;
    atomic {
        run LKP(1);
        run LKP(2);
        run EV();
        run FIN()
    }
}

#ifdef LIVENESS
/* Liveness: every orphaned slot (a memory-tier slot whose dispatch-map entry
 * is not MemoryTier) is eventually re-united with an MT entry or freed —
 * i.e. no permanent leak. Run with pan -a -f (weak fairness). */
ltl no_permanent_orphan { [] (orphan -> <> (!orphan)) }
#endif
