/*
 * System-level model: Cold-path Promotion Atomicity (v3, J20 sync)
 *
 * Verifies the SINGLE-KEY cold path
 *   IDispatcher::lookup -> lookup_async -> promote_and_serve
 *     -> evict_and_insert (evict_for_space / evict_one_clean / mt.insert)
 *     -> pipelined SSD read into the slot
 *     -> dm.promote_block_to_memory_tier   (IN-PLACE BlockDevice->MemoryTier)
 * racing against itself (several lookup threads on one key), against the
 * memory-tier demoter (dm.try_evict_to_block, THEN a separate mt.remove) and
 * against the SSD-capacity evictor (dm.lookup/release_read, mt.remove,
 * dm.remove). The batch_lookup in-place race is covered by the sibling model
 * promotion-lookup-race; it is not modelled here.
 *
 * v2 modelled an old remove+recreate promotion (dm.remove +
 * create_memory_tier_entry + release_write) and an mt.remove undo on SSD-read
 * and create failure. Neither exists on the shipped path any more.
 *
 * Source: origin/unstable @ acd24025 (every line re-derived by symbol, J20).
 *   components/dispatcher/src/lib.rs
 *     lookup                        2197-2208 (lookup_async(..)? 2198)
 *     lookup_async                  2994-3073
 *       dm.lookup(key)              3011
 *       NotExist -> KeyNotFound     3029
 *       MemoryTier hit              3036-3062 (release_read 3056, Ok 3061)
 *       BlockDevice                 3063-3068 (release_read 3064,
 *                                   promote_and_serve(..)? 3065, Ok 3067)
 *       dm.lookup Err -> KeyNotFound 3071
 *     promote_and_serve             750-908
 *       evict_and_insert            768, Err(e) => return Err(e) 789
 *       AllocationFailed && has_drives -> staging lease 770-786
 *       no staging buffer -> AllocationFailed 781-785
 *       no-drives branch            795-818 (LEFT OUT)
 *       channel_pool.checkout()?    830
 *       pipelined read; Err -> return Err(e) 871-884 (883), NO mt.remove
 *       staged -> Ok, no promote    893-896
 *       promote_block_to_memory_tier 903, map_err IoError + ? 904, Ok 907
 *     evict_one_clean               1050-1108 (try_evict_to_block 1066,
 *                                   mt.remove 1068, true 1071, false 1107)
 *     evict_for_space               1123-1168 (AllocationFailed 1149, 1161)
 *     evict_and_insert              1176-1224 (mt.insert 1188, Ok 1189,
 *                                   PoolFull 1190-1207, AlreadyExists 1208-1218)
 *   components/dispatcher/src/background.rs
 *     BackgroundEvictor::evictor_loop 279-370 (get_evictable_offset 323,
 *                                   mt.remove 328, dm.remove 331)
 *     get_evictable_offset          384-405
 *     MemoryTierEvictor::evictor_loop 471-575 (try_evict_to_block 531,
 *                                   mt.remove 535)
 *   components/dispatch-map/src/lib.rs
 *     lookup 128-174, release_read 267-285, remove 334-357,
 *     promote_block_to_memory_tier 482-528, try_evict_to_block 548-580
 *   components/memory-tier/src/lib.rs
 *     insert 328-390 (AlreadyExists 357, PoolFull 363/377, Ok 389),
 *     remove 483-514
 *
 * Safety properties (v2 IDs kept where they still apply):
 *   P1: At most one thread holds a live MT allocation per key at any instant,
 *       and no mt.remove frees a slot an in-flight promoter still owns.
 *   P2: mt.insert(key) succeeds at most once per key lifecycle (lifecycle
 *       ends at mt.remove), and the dispatch-map flips BlockDevice ->
 *       MemoryTier at most once per lifecycle, publishing the flipper's slot.
 *   P3: After all threads complete, read_ref == 0, write_ref == 0, and no
 *       promoter is in flight.
 *   P4: A promoter that returns Err leaves no slot of its own behind; at the
 *       end, a slot exists iff the entry is MemoryTier.
 *   P5: Pool usage counter is always consistent with actual allocations.
 *   P6: (new) lookup() of a key that is in the dispatch-map never returns
 *       AlreadyExists (spec 001 US2 AS-2; contracts/errors.md:34 lists
 *       AlreadyExists only for populate / prepare_store).
 *   P7: (new) A MemoryTier hit is served from a live slot owned by the entry.
 *   P8: (new) The slot holder's in-place promote finds its own BlockDevice
 *       entry (never KeyNotFound/InvalidState -> IoError at 904).
 *
 * Control hooks (all 0 = shipped code; set to 1 only by *_control.pml):
 *   FIX_RECOVER  lookup_async recovers AlreadyExists the way batch_lookup
 *                does (serve_concurrently_promoted, lib.rs:1349-1388).
 *   FIX_UNDO     promote_and_serve frees its slot (mt.remove) on Err after
 *                mt.insert succeeded (read error 883, promote error 904).
 *   FIX_PIN      lookup_async keeps the read pin across promote_and_serve
 *                (release_read moved after it) AND BackgroundEvictor calls
 *                mt.remove only after dm.remove succeeded (like
 *                IDispatcher::remove, lib.rs:3119-3125).
 */

/* ---------- Parameters ---------- */
#define N_THREADS    3    /* concurrent single-key lookup threads           */
#define N_KEYS       2    /* thread t looks up key t % N_KEYS               */
#ifndef POOL_CAP
#define POOL_CAP     2    /* memory-tier capacity in slots                  */
#endif
#ifndef N_LOOKUPS
#define N_LOOKUPS    1    /* lookup() calls per thread (2 = >10M states)    */
#endif
#define MAX_ATTEMPTS 2    /* max_eviction_attempts (default 2048, bounded)  */
#ifndef SSD_EVICTOR
#define SSD_EVICTOR  1    /* 1 = include BackgroundEvictor (background.rs)  */
#endif
#ifndef FIX_RECOVER
#define FIX_RECOVER  0
#endif
#ifndef FIX_UNDO
#define FIX_UNDO     0
#endif
#ifndef FIX_PIN
#define FIX_PIN      0
#endif
#ifndef STRICT_KNF
#define STRICT_KNF   0    /* 1: recovery-timeout KeyNotFound also fails */
#endif

#define OWNER_NONE   0
#define OWNER_DM     (N_THREADS + 1)   /* slot published by a MemoryTier entry */

/* ---------- Dispatch-map entry states ---------- */
mtype = { DM_EMPTY, DM_BLOCK_DEVICE, DM_MEMORY_TIER };
/* dm.lookup results (dispatch-map lib.rs:143, 157, 158) */
mtype = { L_NOTEXIST, L_BD, L_MT };
/* mt.insert results (memory-tier lib.rs:357, 363, 389) */
mtype = { I_OK, I_EXISTS, I_FULL };
/* dm.try_evict_to_block results (dispatch-map lib.rs:553, 559, 569, 576) */
mtype = { TE_OK, TE_ACTIVE_REFS, TE_INVALID_STATE, TE_KEY_NOT_FOUND };
/* dm.promote_block_to_memory_tier results (dispatch-map lib.rs:496, 512, 527) */
mtype = { PB_OK, PB_INVALID_STATE, PB_KEY_NOT_FOUND };
/* IDispatcher::lookup results */
mtype = { R_NONE, R_OK, R_KEY_NOT_FOUND, R_ALREADY_EXISTS, R_IO_ERROR,
          R_ALLOC_FAILED };

/* ---------- Per-key dispatch-map state ---------- */
mtype dm_state[N_KEYS];
byte dm_read_ref[N_KEYS];
byte dm_write_ref[N_KEYS];   /* not taken on the modelled paths (README) */

/* ---------- Per-key memory-tier state ---------- */
bool mt_allocated[N_KEYS];
byte mt_pool_used = 0;
byte slot_owner[N_KEYS];     /* OWNER_NONE / tid+1 (in flight) / OWNER_DM */

/* ---------- Ghost state ---------- */
byte inserts_in_gen[N_KEYS];    /* P2: successful mt.insert this lifecycle */
byte promotion_success[N_KEYS]; /* P2: BD->MT flips this lifecycle         */
byte in_flight[N_KEYS];         /* P1: threads holding an unpublished slot */

byte threads_done = 0;
bool shutdown = false;

/* ---------- mt.remove(key) by a third party (memory-tier lib.rs:483-514) */
/* Result is ignored at every call site (`let _ =` / `.is_err() continue`). */
inline mt_remove(k)
{
    atomic {
        if
        :: mt_allocated[k] ->
            /* P1: never free a slot an in-flight promoter still owns
             * (a slot leaked by an Err return is not in flight). */
            assert(in_flight[k] == 0);
            mt_allocated[k] = false;
            mt_pool_used--;
            slot_owner[k] = OWNER_NONE;
            inserts_in_gen[k] = 0
        :: else -> skip                /* KeyNotFound (lib.rs:510) */
        fi
    }
}

/* ---------- evict_one_clean (dispatcher lib.rs:1050-1108) ----------
 * for cand in mt.oldest_keys(scan) (1065): try_evict_to_block(cand) (1066);
 * Ok -> mt.remove(cand) as a SEPARATE step (1068), return true (1071);
 * ActiveReferences / other Err -> next candidate (1073-1083);
 * exhausted -> false (1107). LRU order abstracted: any resident key may be
 * in or out of the scanned window.
 */
inline evict_one_clean(freed)
{
    freed = false;
    c = 0;
    do
    :: (c < N_KEYS) ->
        if
        :: mt_allocated[c] ->
            atomic {                         /* dispatch-map 548-580 */
                if
                :: dm_state[c] == DM_EMPTY -> te = TE_KEY_NOT_FOUND   /* 553 */
                :: dm_state[c] != DM_EMPTY &&
                   (dm_read_ref[c] != 0 || dm_write_ref[c] != 0) ->
                    te = TE_ACTIVE_REFS                               /* 559 */
                :: dm_state[c] == DM_MEMORY_TIER &&
                   dm_read_ref[c] == 0 && dm_write_ref[c] == 0 ->
                    dm_state[c] = DM_BLOCK_DEVICE;                    /* 568 */
                    promotion_success[c] = 0;
                    te = TE_OK
                :: dm_state[c] == DM_BLOCK_DEVICE &&
                   dm_read_ref[c] == 0 && dm_write_ref[c] == 0 ->
                    te = TE_INVALID_STATE                             /* 576 */
                fi
            };
            if
            :: te == TE_OK ->
                mt_remove(c);                                         /* 1068 */
                freed = true;
                break
            :: else -> c++
            fi
        :: true -> c++                       /* outside the scan window */
        fi
    :: (c >= N_KEYS) -> break
    od;
    atomic { c = 0; te = TE_OK }    /* reset locals (no semantic effect) */
}

/* ---------- Memory-tier demoter ----------
 * MemoryTierEvictor::evictor_loop (background.rs:471-575) runs the same
 * try_evict_to_block (531) then mt.remove (535) sequence as evict_one_clean.
 */
proctype Evictor()
{
    byte c;
    bool freed;
    mtype te;

    do
    :: !shutdown && mt_pool_used > 0 ->    /* utilization >= threshold, 495 */
        evict_one_clean(freed);
        freed = false
    :: shutdown -> break
    od
}

#if SSD_EVICTOR
/* ---------- SSD-capacity evictor (background.rs:279-370) ----------
 * Runs only when SSD utilization >= ssd_eviction_threshold (0.9 default,
 * idispatcher.rs:110; started at lib.rs:2038-2055).
 */
proctype SsdEvictor()
{
    byte k;
    bool bd;
    bool removed;

    do
    :: !shutdown ->
        if
        :: k = 0
        :: k = N_KEYS - 1
        fi;
        /* get_evictable_offset (384-405): dm.lookup (388) + release_read
         * (390/395/399). The pin is taken and dropped in one step. */
        bd = false;
        atomic {
            (dm_write_ref[k] == 0);
            if
            :: dm_state[k] == DM_BLOCK_DEVICE -> bd = true   /* Some(offset) */
            :: else -> skip                                  /* None */
            fi
        };
        if
        :: bd ->
#if FIX_PIN
            /* control: dm.remove first, mt.remove only on success */
            atomic {
                removed = false;
                if
                :: dm_state[k] != DM_EMPTY &&
                   dm_read_ref[k] == 0 && dm_write_ref[k] == 0 ->
                    dm_state[k] = DM_EMPTY;
                    promotion_success[k] = 0;
                    removed = true
                :: else -> skip
                fi
            };
            if
            :: removed -> mt_remove(k)
            :: else -> skip
            fi
#else
            mt_remove(k);                    /* 328 "no-op if not present" */
            atomic {                         /* 331 dm.remove (334-357) */
                if
                :: dm_state[k] != DM_EMPTY &&
                   dm_read_ref[k] == 0 && dm_write_ref[k] == 0 ->
                    dm_state[k] = DM_EMPTY;
                    promotion_success[k] = 0
                :: else -> skip              /* ActiveReferences -> continue */
                fi
            }
#endif
        :: else -> skip
        fi;
        atomic { k = 0; bd = false; removed = false }  /* reset locals */
    :: shutdown -> break
    od
}
#endif

/* ---------- IDispatcher::lookup (single key) ---------- */
proctype LookupThread(byte tid)
{
    byte k;
    byte n;
    byte c;
    byte tries;
    byte ea;
    bool freed;
    bool alloc_failed;
    mtype te;
    mtype lres;
    mtype ins;
    mtype pres;
    mtype ret;

    k = tid % N_KEYS;
    n = 0;
    do
    :: (n < N_LOOKUPS) ->
        n++;
        ret = R_NONE;

        /* dm.lookup(key): dispatcher 3011 -> dispatch-map 128-174.
         * Waits for write_ref == 0 (Timeout left out), takes read_ref. */
        atomic {
            (dm_write_ref[k] == 0);
            if
            :: dm_state[k] == DM_EMPTY -> lres = L_NOTEXIST          /* 143 */
            :: dm_state[k] == DM_BLOCK_DEVICE ->
                dm_read_ref[k]++; lres = L_BD                        /* 157 */
            :: dm_state[k] == DM_MEMORY_TIER ->
                dm_read_ref[k]++; lres = L_MT                        /* 158 */
            fi
        };

        if
        :: lres == L_NOTEXIST ->
            ret = R_KEY_NOT_FOUND                                    /* 3029 */

        :: lres == L_MT ->
            /* serve_memory_tier_to_gpu 3048-3055, under the read pin. */
            assert(mt_allocated[k] && slot_owner[k] == OWNER_DM);    /* P7 */
            atomic {                                                 /* 3056 */
                assert(dm_read_ref[k] > 0);
                dm_read_ref[k]--
            };
            ret = R_OK                                               /* 3061 */

        :: lres == L_BD ->
#if !FIX_PIN
            atomic {                                                 /* 3064 */
                assert(dm_read_ref[k] > 0);
                dm_read_ref[k]--
            };
#endif
            /* ===== promote_and_serve (750-908) ===== */
            /* evict_and_insert (1176-1224) */
            alloc_failed = false;
            tries = 0;
            do
            :: true ->
                /* evict_for_space (1123-1168): while used + needed > cap */
                ea = 0;
                do
                :: (mt_pool_used >= POOL_CAP) ->
                    ea++;
                    if
                    :: ea > MAX_ATTEMPTS -> alloc_failed = true; break   /* 1149 */
                    :: else -> skip
                    fi;
                    evict_one_clean(freed);                              /* 1157 */
                    if
                    :: !freed -> alloc_failed = true; break              /* 1161 */
                    :: else -> skip
                    fi
                :: else -> break                                         /* 1167 */
                od;
                if
                :: alloc_failed -> break
                :: else -> skip
                fi;

                atomic {                                       /* mt.insert 1188 */
                    if
                    :: mt_allocated[k] -> ins = I_EXISTS                  /* 357 */
                    :: !mt_allocated[k] && mt_pool_used >= POOL_CAP ->
                        ins = I_FULL                                      /* 363 */
                    :: !mt_allocated[k] && mt_pool_used < POOL_CAP ->
                        assert(in_flight[k] == 0);                        /* P1 */
                        assert(inserts_in_gen[k] == 0);                   /* P2 */
                        inserts_in_gen[k] = 1;
                        mt_allocated[k] = true;
                        mt_pool_used++;
                        slot_owner[k] = tid + 1;
                        in_flight[k]++;
                        ins = I_OK                                        /* 389 */
                    fi
                };
                if
                :: ins == I_OK -> break                                   /* 1189 */
                :: ins == I_EXISTS -> break                               /* 1217 */
                :: ins == I_FULL ->                                 /* 1190-1207 */
                    tries++;
                    if
                    :: tries >= MAX_ATTEMPTS -> alloc_failed = true; break  /* 1193 */
                    :: else ->
                        evict_one_clean(freed);                           /* 1201 */
                        if
                        :: !freed -> alloc_failed = true; break           /* 1202 */
                        :: else -> skip
                        fi
                    fi
                fi
            od;

            if
            :: alloc_failed ->
                /* AllocationFailed && has_drives (770): staging lease. */
                if
                :: ret = R_OK              /* staged serve, Ok at 895       */
                :: ret = R_IO_ERROR        /* staged read error, 883        */
                :: ret = R_ALLOC_FAILED    /* no staging buffer, 781-785    */
                fi

            :: !alloc_failed && ins == I_EXISTS ->
                /* Err(AlreadyExists) 1217 -> `Err(e) => return Err(e)` 789
                 * -> `?` at 3065 -> `?` at 2198: reaches the caller. */
                ret = R_ALREADY_EXISTS

            :: !alloc_failed && ins == I_OK ->
                /* channel checkout 830 + pipelined SSD read 871-884 */
                if
                :: skip                                   /* read Ok */
                :: ret = R_IO_ERROR                       /* return Err(e) 883 */
                fi;
                if
                :: ret == R_NONE ->
                    /* promote_block_to_memory_tier 903 (dispatch-map 482-528) */
                    atomic {
                        if
                        :: dm_state[k] == DM_BLOCK_DEVICE ->
                            assert(slot_owner[k] == tid + 1);              /* P1 */
                            assert(promotion_success[k] == 0);             /* P2 */
                            dm_state[k] = DM_MEMORY_TIER;                  /* 504 */
                            slot_owner[k] = OWNER_DM;
                            promotion_success[k]++;
                            in_flight[k]--;
                            pres = PB_OK                                   /* 527 */
                        :: dm_state[k] == DM_MEMORY_TIER ->
                            pres = PB_INVALID_STATE                        /* 512 */
                        :: dm_state[k] == DM_EMPTY ->
                            pres = PB_KEY_NOT_FOUND                        /* 496 */
                        fi
                    };
                    assert(pres == PB_OK);                                 /* P8 */
                    if
                    :: pres == PB_OK -> ret = R_OK                         /* 907 */
                    :: else -> ret = R_IO_ERROR                            /* 904 */
                    fi
                :: else -> skip
                fi;
                if
                :: ret != R_OK ->
#if FIX_UNDO
                    /* control: free our own slot before returning Err */
                    atomic {
                        if
                        :: slot_owner[k] == tid + 1 ->
                            mt_allocated[k] = false;
                            mt_pool_used--;
                            slot_owner[k] = OWNER_NONE;
                            inserts_in_gen[k] = 0
                        :: else -> skip
                        fi;
                        in_flight[k]--
                    };
#else
                    atomic { in_flight[k]-- };
#endif
                    /* P4: an Err return leaves no slot of ours behind */
                    assert(slot_owner[k] != tid + 1)
                :: else -> skip
                fi
            fi;

#if FIX_RECOVER
            /* control: serve_concurrently_promoted (1349-1388) */
            if
            :: ret == R_ALREADY_EXISTS ->
                do
                :: atomic {                                   /* dm.lookup 1361 */
                       (dm_write_ref[k] == 0);
                       if
                       :: dm_state[k] == DM_EMPTY -> lres = L_NOTEXIST
                       :: dm_state[k] == DM_BLOCK_DEVICE ->
                           dm_read_ref[k]++; lres = L_BD
                       :: dm_state[k] == DM_MEMORY_TIER ->
                           dm_read_ref[k]++; lres = L_MT
                       fi
                   };
                   if
                   :: lres == L_MT ->
                       assert(mt_allocated[k] && slot_owner[k] == OWNER_DM); /* P7 */
                       atomic { dm_read_ref[k]-- };
                       ret = R_OK; break                       /* 1363-1367 */
                   :: lres == L_BD ->
                       atomic { dm_read_ref[k]-- };            /* 1372 */
                       if
                       :: skip                                 /* back off, retry */
                       :: ret = R_KEY_NOT_FOUND; break         /* 5 s timeout 1373 */
                       fi
                   :: lres == L_NOTEXIST ->
                       ret = R_KEY_NOT_FOUND; break            /* 1385 */
                   fi
                od
            :: else -> skip
            fi;
#endif

#if FIX_PIN
            atomic {                     /* control: release_read after promote */
                assert(dm_read_ref[k] > 0);
                dm_read_ref[k]--
            };
#endif
            skip
        fi;

        assert(ret != R_ALREADY_EXISTS);                                   /* P6 */
        /* KeyNotFound only for an absent entry (3029), or the recovery
         * timeout in the control (1373). */
        assert(ret != R_KEY_NOT_FOUND || lres == L_NOTEXIST ||
               (FIX_RECOVER && !STRICT_KNF));
        /* every path sets a result */
        assert(ret != R_NONE);
        /* reset locals (state-space hygiene, no semantic effect) */
        atomic { ret = R_NONE; lres = L_NOTEXIST; ins = I_OK; pres = PB_OK;
                 te = TE_OK; c = 0; tries = 0; ea = 0; freed = false;
                 alloc_failed = false }

    :: (n >= N_LOOKUPS) -> break
    od;

    threads_done++
}

/* ---------- Initialization ---------- */
init
{
    byte k;
    byte t;
    byte mt_count;

    /* All keys start as persisted BlockDevice entries (demoted earlier). */
    d_step {
        k = 0;
        do
        :: (k < N_KEYS) ->
            dm_state[k] = DM_BLOCK_DEVICE;
            dm_read_ref[k] = 0;
            dm_write_ref[k] = 0;
            mt_allocated[k] = false;
            slot_owner[k] = OWNER_NONE;
            promotion_success[k] = 0;
            inserts_in_gen[k] = 0;
            in_flight[k] = 0;
            k++
        :: (k >= N_KEYS) -> break
        od
    };

    atomic {
        run Evictor();
#if SSD_EVICTOR
        run SsdEvictor();
#endif
        t = 0;
        do
        :: (t < N_THREADS) ->
            run LookupThread(t);
            t++
        :: (t >= N_THREADS) -> break
        od
    };

    (threads_done == N_THREADS);
    shutdown = true;
    timeout;

    d_step {
        k = 0;
        do
        :: (k < N_KEYS) ->
            assert(dm_write_ref[k] == 0);                           /* P3 */
            assert(dm_read_ref[k] == 0);                            /* P3 */
            assert(in_flight[k] == 0);                              /* P3 */
            if
            :: (dm_state[k] == DM_MEMORY_TIER) ->                   /* P2/P4 */
                assert(mt_allocated[k] && slot_owner[k] == OWNER_DM)
            :: (dm_state[k] != DM_MEMORY_TIER) ->
                assert(!mt_allocated[k])
            fi;
            k++
        :: (k >= N_KEYS) -> break
        od;

        mt_count = 0;                                               /* P5 */
        k = 0;
        do
        :: (k < N_KEYS) ->
            if
            :: mt_allocated[k] -> mt_count++
            :: else -> skip
            fi;
            k++
        :: (k >= N_KEYS) -> break
        od;
        assert(mt_pool_used == mt_count)
    }
}
