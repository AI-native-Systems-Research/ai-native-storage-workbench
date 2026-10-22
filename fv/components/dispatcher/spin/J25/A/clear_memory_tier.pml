/*
 * Component-level model: clear_memory_tier (Certus dispatcher)
 * =============================================================
 *
 * Verified: the interaction of DispatcherComponent::clear_memory_tier
 * (code/components/dispatcher/src/lib.rs:3794-3825) with
 *   (1) a client lookup that holds a dispatch-map read pin on a memory-tier
 *       entry (lookup_async, lib.rs:3011-3062), and
 *   (2) the background write-through writer (populate -> copy_gpu_to_memory_
 *       completed -> WriteJob -> process_write_job, lib.rs:3143-3207,
 *       3489-3568, 1390-1465; background.rs:25-143).
 *
 * This file models the CURRENT CODE faithfully (the "original" build).
 * The known-bad behaviour it is expected to expose:
 *   clear_memory_tier's force-remove path (lib.rs:3816-3820) calls
 *   mt.remove(key) BEFORE dm.remove(key), and does so even when
 *   dm.try_evict_to_block(key) failed with ActiveReferences (a reader or
 *   writer still holds a pin). memory-tier remove (memory-tier/src/lib.rs:
 *   483-514) does not check any pin, so the DRAM slot is freed while a
 *   pinned reader/writer still points at it, and dm.remove then fails with
 *   ActiveReferences (swallowed by `let _ =`), leaving a dispatch-map entry
 *   that points at a released memory slot.
 *
 * Properties:
 *   (a) no dispatch-map entry points at a released memory slot;
 *   (b) after clear_memory_tier returns, every entry is BlockDevice with an
 *       SSD offset, or is gone;
 *   (c) no pin is released by an actor that did not take it.
 *
 * Assertion families can be disabled individually for attribution:
 *   spin -DNO_A|-DNO_B|-DNO_C -a clear_memory_tier.pml
 *
 * Run (see Makefile):
 *   spin -a clear_memory_tier.pml
 *   cc -O2 -DMEMLIM=16000 -DSAFETY -o pan pan.c
 *   systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 \
 *     -p OOMPolicy=continue timeout 1800 ./pan -m100000
 */

#define NKEYS 2

mtype = { ABSENT, MT, BD };

/* ---- dispatch-map entry state (dispatch-map/src/entry.rs, state.rs) ---- */
mtype loc[NKEYS];      /* Location: ABSENT = no entry, MT = MemoryTier, BD = BlockDevice */
bool persisted[NKEYS]; /* Location::MemoryTier{ ssd_offset: Some(_) } (write-through landed) */
byte rr[NKEYS];        /* DispatchEntry::read_ref (dispatch-map/src/lib.rs:147) */

/* ---- memory-tier pool state (memory-tier/src/lib.rs:70-115) ---- */
bool slot[NKEYS];      /* pool.slots contains key */
byte gen[NKEYS];       /* slot generation: bumped on every free, ABA detector */

/* ---- observability ---- */
bool clr_done;         /* clear_memory_tier returned Ok(count) (lib.rs:3824) */
byte done_count;       /* number of bounded actors (LKP, WTR) that finished */
#define actors_done (done_count == 2)

/* Per-family assertion switches (all on by default) */
#ifdef NO_A
#define A_CHECK(cond) skip
#else
#define A_CHECK(cond) assert(cond)
#endif
#ifdef NO_B
#define B_CHECK(cond) skip
#else
#define B_CHECK(cond) assert(cond)
#endif
#ifdef NO_C
#define C_CHECK(cond) skip
#else
#define C_CHECK(cond) assert(cond)
#endif

/* Property (a): a dispatch-map entry in MemoryTier state must always point
 * at a live memory-tier slot. Checked after every shared-state transition. */
#define INV(k) A_CHECK(loc[k] != MT || slot[k])

/* dm.try_evict_to_block — dispatch-map/src/lib.rs:548-580.
 * Ok only when the entry is MemoryTier, unpinned (read_ref == 0 and
 * write_ref == 0, lib.rs:558) and persisted (ssd_offset set, lib.rs:562-569).
 * write_ref is collapsed into the atomic store prologue (see WTR, Left out). */
inline dm_try_evict_to_block(k, ok) {
    atomic {
        if
        :: (loc[k] == MT && rr[k] == 0 && persisted[k]) ->
            loc[k] = BD;            /* demote; SSD offset retained (lib.rs:567-568) */
            ok = true
        :: else ->
            ok = false              /* KeyNotFound | ActiveReferences | InvalidState */
        fi;
        INV(k)
    }
}

/* mt.remove — memory-tier/src/lib.rs:483-514. Removes the slot and
 * deallocates the offset with NO pin check (lib.rs:507-513). */
inline mt_remove(k) {
    atomic {
        if
        :: slot[k] ->
            slot[k] = false;
            gen[k] = gen[k] + 1     /* deallocate: the pointer is now dangling */
        :: else ->
            skip                    /* KeyNotFound, swallowed by `let _ =` */
        fi;
        INV(k)
    }
}

/* dm.remove — dispatch-map/src/lib.rs:334-357.
 * Refuses with ActiveReferences while read_ref > 0 (lib.rs:341-343). */
inline dm_remove(k) {
    atomic {
        if
        :: (loc[k] != ABSENT && rr[k] == 0) ->
            loc[k] = ABSENT
        :: else ->
            skip                    /* ActiveReferences, swallowed: lib.rs:3819 */
        fi;
        INV(k)
    }
}

/* dm.lookup — dispatch-map/src/lib.rs:128-174. Takes a read pin (lib.rs:147)
 * and returns the location snapshot. NotExist takes no pin (lib.rs:143). */
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

/* dm.release_read — dispatch-map/src/lib.rs:267-285.
 * Property (c): only the actor that took the pin may release it. */
inline dm_release_read(k, mypin) {
    atomic {
        C_CHECK(mypin);
        if
        :: mypin -> rr[k] = rr[k] - 1; mypin = false
        :: else -> skip            /* unreachable when C_CHECK is on */
        fi;
        INV(k)
    }
}

/* dm.convert_to_storage — dispatch-map/src/lib.rs:176-208. Sets ssd_offset
 * AND decrements read_ref (lib.rs:194-196): this is how the writer's pin is
 * released on the write-through success path (dispatcher lib.rs:1464). */
inline dm_convert_to_storage(k, mypin) {
    atomic {
        C_CHECK(mypin);
        if
        :: (loc[k] == MT && mypin) ->
            persisted[k] = true;
            rr[k] = rr[k] - 1;
            mypin = false
        :: else ->
            skip                    /* InvalidState; pin leaks (lib.rs:1464 `let _ =`) */
        fi;
        INV(k)
    }
}

/* The pinned use of a slot: serve_memory_tier_to_gpu DMAs from `pointer`
 * while the caller's read pin is held (dispatcher lib.rs:3048-3055). The pin
 * is the ONLY thing that must keep the slot alive (dispatcher/src/pins.rs:8-15).
 * Property (a), use side: the slot must still be the same live allocation. */
inline use_slot(k, snap) {
    A_CHECK(slot[k] && gen[k] == snap)
}

/*
 * Actor 1: clear_memory_tier — dispatcher/src/lib.rs:3794-3825.
 * loop { candidates = mt.oldest_keys(64); if empty break;
 *        for key: if dm.try_evict_to_block(key).is_ok() { mt.remove(key) }
 *                 else { mt.remove(key); dm.remove(key) } }
 */
proctype CLR() {
    byte k;
    bool evict_ok;
    do
    :: (slot[0] || slot[1]) ->          /* mt.oldest_keys non-empty (lib.rs:3809-3812) */
        if
        :: slot[0] -> k = 0
        :: slot[1] -> k = 1
        fi;
        /* The dm call and the mt call hold DIFFERENT locks in the real code:
         * the window between them stays open. */
        dm_try_evict_to_block(k, evict_ok);      /* lib.rs:3814 */
        if
        :: evict_ok ->
            mt_remove(k)                        /* lib.rs:3815 */
        :: else ->
            /* "Entry not evictable (no SSD copy) — force remove." lib.rs:3817 */
            mt_remove(k);                       /* lib.rs:3818 — slot freed FIRST */
            dm_remove(k)                        /* lib.rs:3819 — fails if pinned */
        fi
    :: else -> break                            /* candidates empty (lib.rs:3810-3811) */
    od;
    atomic {
        clr_done = true;                        /* Ok(count), lib.rs:3824 */
        /* Property (b): after clear_memory_tier, every entry is BlockDevice
         * with an SSD offset, or is gone. Exemption: a store that began
         * AFTER clear drained the tier may legitimately have re-created a
         * MemoryTier entry — clear returns only when mt.oldest_keys is
         * empty (lib.rs:3809-3811), so any entry with a LIVE slot at return
         * is such a fresh store. A clear-leftover entry is MT with NO slot. */
        B_CHECK((loc[0] != MT || slot[0]) && (loc[1] != MT || slot[1]))
    }
}

/* mt.touch — memory-tier/src/lib.rs:516-529 (eviction-order refresh only). */
inline mt_touch(k) {
    atomic { skip }
}

/*
 * Actor 2: a client lookup that holds a pin — lookup_async MemoryTier hit,
 * dispatcher/src/lib.rs:3036-3062. Bounded to 2 operations.
 */
proctype LKP() {
    byte k;
    bool hit;
    bool mypin = false;
    byte snap = 0;
    byte i = 0;
    do
    :: i < 2 ->
        i = i + 1;
        if
        :: k = 0
        :: k = 1
        fi;
        dm_lookup(k, hit, mypin, snap);         /* lib.rs:3011 / 3036 */
        if
        :: hit ->
            use_slot(k, snap);                  /* H2D DMA under pin, lib.rs:3048 */
            dm_release_read(k, mypin);          /* lib.rs:3056 */
            mt_touch(k)                         /* lib.rs:3057 */
        :: mypin ->
            /* BlockDevice hit: pin released, cold path out of scope (lib.rs:3063-3065) */
            dm_release_read(k, mypin)
        :: else ->
            skip                                /* NotExist, lib.rs:3029 */
        fi
    :: else -> break
    od;
    atomic { done_count = done_count + 1 }
}

/*
 * Actor 3: the background write-through writer. Models the store path
 * populate (lib.rs:3159-3198) -> copy_gpu_to_memory_completed (lib.rs:3489-
 * 3568) -> WriteJob enqueue (lib.rs:3559-3566, background.rs:78-86) ->
 * process_write_job (lib.rs:1390-1465). Bounded to 2 store cycles.
 */
proctype WTR() {
    byte k;
    bool mypin = false;
    byte snap = 0;
    bool peek_ok;
    byte i = 0;
    do
    :: i < 2 ->
        i = i + 1;
        if
        :: k = 0
        :: k = 1
        fi;
        /* Store prologue, atomic in the model (Left out: the create(wr=1) ->
         * downgrade(wr->rr) window, lib.rs:3508-3552):
         *   mt.insert                     (memory-tier lib.rs:328-390)
         *   dm.create_memory_tier_entry   (dispatch-map lib.rs:393-439, wr = 1)
         *   dm.downgrade_reference        (dispatch-map lib.rs:307-332, wr -> rr)
         *   writer.enqueue(WriteJob)      (background.rs:78-86)
         * The installed read pin is the write-through pin that
         * process_write_job releases via convert_to_storage / release_read. */
        atomic {
            if
            :: (loc[k] == ABSENT && !slot[k]) ->
                slot[k] = true;
                gen[k] = gen[k] + 1;
                snap = gen[k];
                loc[k] = MT;
                persisted[k] = false;
                rr[k] = rr[k] + 1;
                mypin = true
            :: else ->
                skip                /* populate declines: AlreadyExists etc. */
            fi;
            INV(k)
        };
        if
        :: mypin ->
            /* process_write_job (lib.rs:1390-1465) */
            atomic { peek_ok = slot[k] };       /* mt.peek, lib.rs:1399 */
            if
            :: peek_ok ->
                /* DMA mem_ptr -> SSD, lib.rs:1420-1451: the writer's read pin
                 * (installed by downgrade_reference) must keep the slot alive. */
                use_slot(k, snap);
                if
                :: /* SSD write ok */
                    dm_convert_to_storage(k, mypin)   /* lib.rs:1464 */
                :: /* SSD write failed */
                    dm_release_read(k, mypin)         /* lib.rs:1454 */
                fi
            :: else ->
                /* peek -> None: release pin, drop job (lib.rs:1401-1404) */
                dm_release_read(k, mypin)
            fi
        :: else -> skip
        fi
    :: else -> break
    od;
    atomic { done_count = done_count + 1 }
}

init {
    /* Both keys start resident in the memory tier, as stored earlier by a
     * client; whether write-through has already landed is nondeterministic
     * (persisted = ssd_offset Some/None). */
    loc[0] = MT; loc[1] = MT;
    slot[0] = true; slot[1] = true;
    rr[0] = 0; rr[1] = 0;
    if
    :: persisted[0] = true
    :: persisted[0] = false
    fi;
    if
    :: persisted[1] = true
    :: persisted[1] = false
    fi;
    atomic {
        run CLR();
        run LKP();
        run WTR()
    }
}

#ifdef LIVENESS
/* Liveness: once both bounded actors are done (all pins they took have been
 * released), clear_memory_tier must eventually return. */
ltl clear_terminates { [] (actors_done -> <> clr_done) }
#endif
