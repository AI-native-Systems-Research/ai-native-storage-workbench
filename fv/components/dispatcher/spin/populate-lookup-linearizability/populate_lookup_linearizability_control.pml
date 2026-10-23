/*
 * CONTROL COPY of populate_lookup_linearizability.pml (J20). Identical model
 * with the two minimal code fixes switched on:
 *   FIX_SYNC_LEAK : populate (lib.rs:3193-3194) releases its reserved
 *                   memory-tier slot when stream_synchronize fails, exactly as
 *                   batch_populate already does (lib.rs:3297).
 *   FIX_COLD_PIN  : lookup_async keeps the dm.lookup read-ref across
 *                   promote_and_serve (move `dm.release_read(key)` from
 *                   lib.rs:3064 to after the promote_and_serve call at 3065,
 *                   on both the Ok and Err paths).
 * No assert is changed or removed.
 */
#define FIX_SYNC_LEAK
#define FIX_COLD_PIN
/*
 * System-level model: Populate-Lookup Linearizability
 *
 * Synced (J20) to origin/unstable @ acd24025. Every lib.rs:NNNN below is
 * components/dispatcher/src/lib.rs at that commit unless another file is named;
 * dm/lib.rs = components/dispatch-map/src/lib.rs, bg.rs =
 * components/dispatcher/src/background.rs. All citations were re-derived by
 * symbol (grep -n) on acd24025, not copied from the previous model.
 *
 * Verifies the Certus dispatcher guarantee (strict form):
 *
 *   After populate(key) returns Ok AND cached the block, a lookup(key) whose
 *   dm.lookup happens afterwards NEVER returns KeyNotFound, UNLESS an explicit
 *   remove(key) removed the dispatch-map entry in between.
 *
 * The memory-tier evictor NO LONGER has a drop branch (spec dispatcher 001
 * FR-024 l.301, commit 1d55b9c2): evict_one_clean (lib.rs:1050-1108) only
 * demotes (dm.try_evict_to_block, lib.rs:1066) and skips unpersisted victims.
 * So the only departure path modelled is remove().
 *
 * populate's REAL return value is modelled in pop_ret[k]:
 *   POP_OK_CACHED  lib.rs:3206  Ok(()) after copy_gpu_to_memory_completed
 *   POP_OK_DROPPED lib.rs:3174  Ok(()) on AllocationFailed (drop-on-full, FR-060)
 *   POP_ERR_IO     lib.rs:3476-3477 (D2H submit fail) / 3193-3194 (sync fail)
 * Spec FR-060 (spec 001 l.356-358) and FR-003 (l.280) explicitly allow a lookup
 * after a drop-on-full Ok to miss. The default build checks the spec contract
 * (P-LIN excludes POP_OK_DROPPED, P-DROP asserts no entry was left). Build with
 * -DLITERAL_OK to check the literal "Ok => no miss" contract instead.
 *
 * Fix switches (all off here; the sibling *_control.pml turns them on):
 *   FIX_SYNC_LEAK       populate releases its slot when stream_synchronize
 *                       fails (as batch_populate already does, lib.rs:3297).
 *   FIX_COLD_PIN        lookup_async keeps the dm.lookup read-ref across
 *                       promote_and_serve (move release_read lib.rs:3064 to
 *                       after 3065), so remove()/SSD-evictor dm.remove refuse
 *                       the key while a cold promote is reading into its slot.
 *   FIX_SSD_EVICT_ORDER SSD evictor calls dm.remove before mt.remove and frees
 *                       the slot only if dm.remove succeeded (bg.rs:328/331).
 *
 * Diagnostic switch (not a fix, never in the default build):
 *   NO_SLOT_CHECK       drops P-CSLOT and the P-FIN orphan-slot assert, only to
 *                       show that P-LIN/P-PIN/P-WJOB/P-DROP hold on today's
 *                       code once the slot findings are set aside.
 *   SSD_EVICTOR         adds the SSD-capacity evictor (bg.rs:279) as a second
 *                       departure path (spec 001 FR-031). Off by default.
 *
 * Wiring modelled:
 *   populate  lib.rs:3143 -> reserve_memory 3159 (-> evict_and_insert 1176
 *             -> evict_for_space 1123 -> evict_one_clean 1050; mt.insert 1188)
 *             -> copy_gpu_to_memory_async 3192 / stream_synchronize 3193
 *             -> copy_gpu_to_memory_completed 3198:
 *                  dm.create_memory_tier_entry 3508 (VISIBLE, write_ref=1)
 *                  dm.downgrade_reference      3552 (write_ref -> writer read_ref)
 *                  writer.enqueue              3561
 *             -> Ok(()) 3206
 *   lookup    lib.rs:2197 -> lookup_async 2994: dm.lookup 3011
 *             (dm/lib.rs:128; waits <= 2 s for write_ref==0, dm/lib.rs:133,
 *             Timeout dm/lib.rs:138 -> KeyNotFound lib.rs:3071)
 *             MemoryTier 3036: serve 3048, release_read 3056, Ok 3061
 *             BlockDevice 3063: release_read 3064 (UNPINNED from here),
 *             promote_and_serve 3065 (lib.rs:750: evict_and_insert 768,
 *             staging fallback 770-787, NVMe+H2D 851-876,
 *             staged return 884-887, promote_block_to_memory_tier 903-904)
 *   writer    process_write_job lib.rs:1390: mt.peek 1399, release_read on
 *             failure 1402/1432/1442/1454, convert_to_storage 1464
 *             (dm/lib.rs:176, consumes the read-ref)
 *   evictor   evict_one_clean lib.rs:1050: try_evict_to_block 1066
 *             (dm/lib.rs:548), mt.remove 1068 (separate step);
 *             MemoryTierEvictor bg.rs:471: try_evict_to_block bg.rs:531,
 *             mt.remove bg.rs:535 (same two-step shape)
 *   ssd-evict BackgroundEvictor::evictor_loop bg.rs:279: dm.oldest_keys 314,
 *             get_evictable_offset 322 (bg.rs:384: dm.lookup, BlockDevice
 *             only, release_read 390), mt.remove 328, dm.remove 331,
 *             remove_extent 348
 *   remove    lib.rs:3096: dm.lookup 3105 + release_read 3107/3111,
 *             Err -> KeyNotFound 3114, dm.remove 3119 (dm/lib.rs:334,
 *             ActiveReferences/KeyNotFound -> KeyNotFound 3120),
 *             mt.remove 3124, remove_extent 3131, Ok 3135
 *
 * Safety properties:
 *   P-LIN  : lookup -> KeyNotFound (NotExist or Timeout) implies, at the
 *            dm.lookup instant, populate had not returned Ok-cached, or an
 *            explicit remove() / SSD-capacity eviction (spec 001 FR-031) had
 *            removed the entry. (Strict: no memory-tier eviction-drop escape.)
 *   P-DROP : populate returned Ok-dropped => no dispatch-map entry (FR-060).
 *   P-PIN  : a MemoryTier hit's entry stays committed, MemoryTier and resident
 *            while the lookup holds its read-ref.
 *   P-REF  : dm.remove / try_evict_to_block only succeed at read_refs==0 and
 *            write_ref==0.
 *   P-WJOB : a queued write-through job finds its entry committed, resident,
 *            MemoryTier, this generation, and pinned by the writer's read-ref.
 *   P-CSLOT: when an (unpinned) cold promote finishes its NVMe->DRAM read, the
 *            DRAM slot it inserted at lib.rs:768 is still allocated.
 *   P-RET  : returned labels agree with the state at the returning line.
 *   P-FIN  : at quiescence: no leaked refs; every memory-tier slot belongs to a
 *            committed MemoryTier entry (no orphan slot); pool count
 *            consistent; Ok-cached and never removed => still committed.
 */

/* ---------- Parameters ---------- */
#define N_KEYS      2
#define POOL_CAP    1
#define N_LOOK      2      /* concurrent Looker processes */
#define LOOKUPS     1      /* lookups per Looker */
#define MAX_TRIES   4      /* bounded reserve attempts before AllocationFailed */

/* ---------- Return labels ---------- */
mtype = { MT, SSD };                                     /* DM location */
mtype = { POP_NONE, POP_OK_CACHED, POP_OK_DROPPED, POP_ERR_IO };
mtype = { L_NONE, L_OK_MT, L_OK_SSD_PROMOTED, L_OK_SSD_STAGED, L_KNF_NOTEXIST,
          L_KNF_TIMEOUT, L_ERR_EXISTS, L_ERR_IO_PROMOTE };
mtype = { R_NONE, R_OK, R_KNF_TIMEOUT, R_KNF_ABSENT, R_KNF_PINNED };

/* ---------- Per-key state ---------- */
bool committed[N_KEYS];    /* dispatch-map entry exists */
mtype location[N_KEYS];    /* MT / SSD (BlockDevice) */
bool persisted[N_KEYS];    /* ssd_offset set (write-through landed) */
byte read_refs[N_KEYS];
bool write_ref[N_KEYS];    /* DM write_ref (create_memory_tier_entry .. downgrade) */
bool in_pool[N_KEYS];      /* memory-tier holds a slot keyed k */
byte slot_id[N_KEYS];      /* which allocation the slot keyed k is (for P-CSLOT) */
byte next_slot = 1;
byte pool_used = 0;
byte gen[N_KEYS];

mtype pop_ret[N_KEYS];     /* populate's returned value */
bool departed[N_KEYS];     /* remove() or SSD evictor: dm.remove succeeded */
mtype rm_ret = R_NONE;

chan write_q = [N_KEYS] of { byte, byte };

byte pop_done = 0;
byte look_done = 0;
bool shutdown = false;

/* mt.insert succeeds: allocate a slot keyed k (memory-tier/src/lib.rs:328). */
inline mt_alloc(k)
{
    in_pool[k] = true;
    pool_used++;
    slot_id[k] = next_slot;
    next_slot = (next_slot % 7) + 1
}

/* mt.remove(k): frees whatever slot is keyed k (memory-tier/src/lib.rs:483). */
inline mt_free(k)
{
    if
    :: in_pool[k] -> in_pool[k] = false; pool_used--; slot_id[k] = 0
    :: else -> skip                       /* KeyNotFound, ignored by callers */
    fi
}

/* ---------- evict_one_clean: demote-only, two steps ---------- */
/*
 * lib.rs:1050-1108. Step 1 = dm.try_evict_to_block (dm/lib.rs:548-580, one DM
 * lock hold): succeeds only for an unpinned (read_ref==0, write_ref==0)
 * MemoryTier entry with ssd_offset set; flips it to BlockDevice. Every other
 * candidate is skipped (ActiveReferences lib.rs:1073 / InvalidState or
 * KeyNotFound lib.rs:1076) -- NO dm.remove (removed in 1d55b9c2, FR-024).
 * Step 2 = mt.remove (lib.rs:1068), a separate memory-tier lock hold.
 * Candidates come from mt.oldest_keys (lib.rs:1057), so only keys with a slot.
 */
inline evict_one_clean(freed)
{
    byte v;
    atomic {
        freed = false;
        v = 0;
        do
        :: (v < N_KEYS) ->
            if
            :: (in_pool[v] && committed[v] && location[v] == MT
                && read_refs[v] == 0 && !write_ref[v] && persisted[v]) ->
                break
            :: else -> v++
            fi
        :: (v >= N_KEYS) -> break
        od;
        if
        :: (v < N_KEYS) ->
            assert(read_refs[v] == 0 && !write_ref[v]);     /* P-REF */
            location[v] = SSD;
            freed = true
        :: else -> skip                     /* record_eviction_scan_exhausted 1106 */
        fi
    };
    if
    :: freed ->
        atomic { mt_free(v) }               /* mt.remove(cand) 1068 */
    :: else -> skip
    fi
}

/* ---------- populate lib.rs:3143-3207 ---------- */
inline do_populate(k)
{
    bool reserved = false;
    byte tries = 0;
    bool freed;

    /* Phase 1: reserve_memory 3159 -> evict_and_insert -> mt.insert 1188. */
    do
    :: !reserved ->
        atomic {
            /* mt.insert AlreadyExists (1208-1217) is unreachable here: one
             * Populator per key, and no other path inserts k before k is
             * committed. Listed under "Left out" in the README. */
            if
            :: (!in_pool[k] && pool_used < POOL_CAP) ->
                mt_alloc(k);                 /* slot only; no DM entry yet */
                reserved = true
            :: else -> skip
            fi
        };
        if
        :: reserved -> skip
        :: !reserved ->
            evict_one_clean(freed);
            if
            :: freed -> skip
            :: !freed ->
                tries++;
                if
                :: (tries >= MAX_TRIES) -> break   /* AllocationFailed 1149/1161 */
                :: else -> skip                    /* backpressure retry 3415 */
                fi
            fi
        fi
    :: reserved -> break
    od;

    if
    :: !reserved ->
        /* lib.rs:3161-3174: drop-on-full, returns Ok(()) without caching. */
        pop_ret[k] = POP_OK_DROPPED;
        assert(!committed[k])                       /* P-DROP (FR-060) */
    :: reserved ->
        /* Phase 2: D2H copy 3192 + stream_synchronize 3193. */
        if
        :: true ->
            skip                                    /* DMA lands */
        :: true ->
            /* memcpy_d2h_async fails: mt.remove 3476, IoError 3477 */
            atomic { mt_free(k); pop_ret[k] = POP_ERR_IO;
                     assert(!committed[k] && !in_pool[k]) };        /* P-RET */
            goto pop_end
        :: true ->
            /* stream_synchronize fails 3193-3194: IoError via `?`. */
            atomic {
#ifdef FIX_SYNC_LEAK
                mt_free(k);
#endif
                pop_ret[k] = POP_ERR_IO;
                assert(!committed[k])                           /* P-RET */
            };
            goto pop_end
        fi;

        /* Phase 3a: dm.create_memory_tier_entry 3508 (dm/lib.rs:393-439):
         * entry VISIBLE with write_ref=1. */
        atomic {
            assert(!committed[k]);
            committed[k] = true;
            location[k] = MT;
            persisted[k] = false;
            write_ref[k] = true;
            gen[k]++
        };
        /* Phase 3b: downgrade_reference 3552 (dm/lib.rs:307) + enqueue 3561
         * + return Ok 3568/3206. Merged: nothing between them changes DM
         * visibility, and setting pop_ret here is no later than the real
         * return (stricter for P-LIN). */
        atomic {
            write_ref[k] = false;
            read_refs[k]++;
            write_q ! k, gen[k];
            pop_ret[k] = POP_OK_CACHED
        }
    fi;
pop_end:
    pop_done++
}

proctype Populator(byte k)
{
    do_populate(k)
}

/* ---------- Background writer: process_write_job lib.rs:1390-1466 ---------- */
proctype BgWriter()
{
    byte jk, jg;
    do
    :: write_q ? jk, jg ->
        atomic {
            /* P-WJOB */
            assert(committed[jk] && gen[jk] == jg && location[jk] == MT);
            assert(in_pool[jk] && read_refs[jk] > 0);
            if
            :: true -> persisted[jk] = true    /* convert_to_storage 1464 */
            :: true -> skip                    /* reserve/write fail 1442/1454 */
            fi;
            read_refs[jk]--
        }
    :: (shutdown && empty(write_q)) -> break
    od
}

/* ---------- MemoryTierEvictor bg.rs:471 / populate-side evict ---------- */
proctype Evictor()
{
    bool freed;
    do
    :: !shutdown -> evict_one_clean(freed)
    :: shutdown -> break
    od
}

/* ---------- SSD-capacity evictor bg.rs:279-370 (one candidate) ---------- */
/* Optional (-DSSD_EVICTOR): off in the default build, which keeps the
 * strict "only remove() departs" form of P-LIN that the task specifies. */
#ifdef SSD_EVICTOR
proctype SsdEvictor()
{
    byte k;
    select(k : 0 .. N_KEYS - 1);           /* dm.oldest_keys bg.rs:314 */

    /* get_evictable_offset bg.rs:322 -> bg.rs:384-404: dm.lookup; only a
     * BlockDevice entry yields Some(offset); pin released at bg.rs:390.
     * Timeout / NotExist / MemoryTier -> None -> continue (bg.rs:324). */
    atomic {
        if
        :: (committed[k] && !write_ref[k] && location[k] == SSD) -> skip
        :: else -> goto ssd_end
        fi
    };
#ifndef FIX_SSD_EVICT_ORDER
    /* mt.remove(key) bg.rs:328 -- BEFORE dm.remove, unconditionally. */
    atomic { mt_free(k) };
#endif
    /* dm.remove(key) bg.rs:331 (dm/lib.rs:334): no location check. */
    atomic {
        if
        :: (committed[k] && read_refs[k] == 0 && !write_ref[k]) ->
            assert(read_refs[k] == 0 && !write_ref[k]);        /* P-REF */
            committed[k] = false;
            departed[k] = true;
#ifdef FIX_SSD_EVICT_ORDER
            mt_free(k);
#endif
            skip
        :: else -> skip                     /* is_err -> continue bg.rs:332 */
        fi
    };
    /* remove_extent bg.rs:348: extents not modelled (Left out). */
ssd_end:
    skip
}
#endif

/* ---------- remove lib.rs:3096-3136 (non-atomic, 3 steps) ---------- */
proctype Remover()
{
    byte k;
    select(k : 0 .. N_KEYS - 1);

    /* Step 1: dm.lookup 3105 + release_read 3107/3111 (merged; remove's own
     * transient pin only makes others' refusals more likely). Blocking wait
     * for write_ref==0, or Timeout (over-approximated as in Looker). */
    if
    :: atomic { (!committed[k] || !write_ref[k]) -> skip }   /* NotExist / hit */
    :: atomic { (committed[k] && write_ref[k]) ->
            rm_ret = R_KNF_TIMEOUT;         /* Err(Timeout) -> KeyNotFound 3114 */
            assert(committed[k] && write_ref[k]) }             /* P-RET */
            ;
            goto rm_end
    fi;
    /* Step 2: dm.remove 3119 (dm/lib.rs:334-357). */
    atomic {
        if
        :: !committed[k] ->
            rm_ret = R_KNF_ABSENT;          /* KeyNotFound -> KeyNotFound 3120 */
            assert(!committed[k]);                              /* P-RET */
            goto rm_end
        :: (committed[k] && (read_refs[k] > 0 || write_ref[k])) ->
            rm_ret = R_KNF_PINNED;          /* ActiveReferences -> KeyNotFound 3120 */
            assert(read_refs[k] > 0 || write_ref[k]);           /* P-RET */
            goto rm_end
        :: (committed[k] && read_refs[k] == 0 && !write_ref[k]) ->
            assert(read_refs[k] == 0 && !write_ref[k]);        /* P-REF */
            committed[k] = false;
            departed[k] = true
        fi
    };
    /* Step 3: mt.remove(key) 3124 -- frees whatever slot is keyed k NOW. */
    atomic {
        mt_free(k);
        /* remove_extent 3131: extents not modelled (Left out). */
        rm_ret = R_OK;                      /* Ok(()) 3135 */
        assert(departed[k])                                     /* P-RET */
    };
rm_end:
    skip
}

/* ---------- lookup / lookup_async lib.rs:2994-3073 ---------- */
proctype Looker()
{
    byte n = 0;
    byte k;
    mtype res;
    mtype ret;
    bool saw_pop_ok;
    bool saw_departed;
    bool mine;
    bool freed;
    byte my_slot;

    do
    :: (n < LOOKUPS) ->
        n++;
        ret = L_NONE;
        select(k : 0 .. N_KEYS - 1);

        /* dm.lookup 3011 -> dm/lib.rs:128-174. Linearization point.
         * wait_for (dm/state.rs:54) returns the guard held, so "predicate
         * satisfied" and the read-ref increment are one atomic step: modelled
         * as a blocking atomic guard. The Timeout branch (2 s of write_ref,
         * dm/lib.rs:25,137-139) is an over-approximation: allowed whenever a
         * write_ref is held at all. */
        if
        :: atomic { (!committed[k] || !write_ref[k]) ->
#ifdef LITERAL_OK
            saw_pop_ok = (pop_ret[k] == POP_OK_CACHED || pop_ret[k] == POP_OK_DROPPED);
#else
            saw_pop_ok = (pop_ret[k] == POP_OK_CACHED);
#endif
            saw_departed = departed[k];
            if
            :: !committed[k] ->
                ret = L_KNF_NOTEXIST                    /* NotExist -> 3029 */
            :: else ->
                res = location[k];
                read_refs[k]++                          /* dm/lib.rs:146-149 */
            fi
           }
        :: atomic { (committed[k] && write_ref[k]) ->
#ifdef LITERAL_OK
            saw_pop_ok = (pop_ret[k] == POP_OK_CACHED || pop_ret[k] == POP_OK_DROPPED);
#else
            saw_pop_ok = (pop_ret[k] == POP_OK_CACHED);
#endif
            saw_departed = departed[k];
            ret = L_KNF_TIMEOUT                         /* dm 138 Timeout -> 3071 */
           }
        fi;

        if
        :: (ret == L_KNF_NOTEXIST || ret == L_KNF_TIMEOUT) ->
            /* P-LIN (strict): miss only if not yet Ok-cached, or removed. */
            assert(!saw_pop_ok || saw_departed)
        :: (ret == L_NONE && res == MT) ->
            /* serve_memory_tier_to_gpu 3048 -- pinned. P-PIN. */
            assert(committed[k] && location[k] == MT && in_pool[k]);
            atomic {
                assert(read_refs[k] > 0);
                read_refs[k]--;                         /* release_read 3056 */
                ret = L_OK_MT                           /* Ok 3061 */
            }
        :: (ret == L_NONE && res == SSD) ->
#ifndef FIX_COLD_PIN
            atomic {
                assert(read_refs[k] > 0);
                read_refs[k]--                          /* release_read 3064 */
            };
#endif
            /* promote_and_serve 3065 (lib.rs:750), entry now UNPINNED. */
            mine = false;
            atomic {
                if
                :: in_pool[k] ->
                    ret = L_ERR_EXISTS;                 /* mt.insert AlreadyExists 1208-1217, 788 */
                    assert(in_pool[k])                  /* P-RET */
                :: (!in_pool[k] && pool_used < POOL_CAP) ->
                    mt_alloc(k); mine = true; my_slot = slot_id[k]   /* 768 Ok */
                :: (!in_pool[k] && pool_used >= POOL_CAP) ->
                    skip                                /* evict_for_space needed */
                fi
            };
            if
            :: (ret == L_NONE && !mine) ->
                evict_one_clean(freed);                 /* evict_for_space 1146 */
                atomic {
                    if
                    :: (freed && in_pool[k]) ->
                        ret = L_ERR_EXISTS;
                        assert(in_pool[k])              /* P-RET */
                    :: (freed && !in_pool[k] && pool_used < POOL_CAP) ->
                        mt_alloc(k); mine = true; my_slot = slot_id[k]
                    :: else ->
                        ret = L_OK_SSD_STAGED;          /* staging lease 770-780 */
                        assert(!mine)                   /* P-RET: no tier slot */
                    fi
                }
            :: else -> skip
            fi;
            if
            :: (ret == L_ERR_EXISTS) ->
                skip                                    /* Err(AlreadyExists) 788 */
            :: (ret == L_OK_SSD_STAGED) ->
                skip                                    /* NVMe->staging->GPU; Ok 886 */
            :: (mine) ->
                /* NVMe -> our DRAM slot -> GPU (851-876): a schedulable gap. */
                skip;
                atomic {
#ifndef NO_SLOT_CHECK
                    assert(in_pool[k] && slot_id[k] == my_slot);   /* P-CSLOT */
#endif
                    if
                    :: (committed[k] && location[k] == SSD) ->
                        location[k] = MT;               /* promote_block_to_memory_tier 903 */
                        ret = L_OK_SSD_PROMOTED;        /* Ok 3067 */
                        assert(committed[k] && location[k] == MT && in_pool[k])   /* P-RET */
                    :: else ->
                        /* KeyNotFound / InvalidState -> IoError 904 via `?`;
                         * the slot inserted at 768 is NOT removed. */
                        ret = L_ERR_IO_PROMOTE;
                        assert(!committed[k] || location[k] != SSD)   /* P-RET */
                    fi
                }
            fi;
#ifdef FIX_COLD_PIN
            atomic {
                assert(read_refs[k] > 0);
                read_refs[k]--                          /* release_read, moved */
            };
#endif
            skip
        fi
    :: (n >= LOOKUPS) -> break
    od;
    look_done++
}

init
{
    byte k;
    byte cnt;

    d_step {
        k = 0;
        do
        :: (k < N_KEYS) ->
            committed[k] = false; location[k] = MT; persisted[k] = false;
            read_refs[k] = 0; write_ref[k] = false; in_pool[k] = false;
            slot_id[k] = 0;
            gen[k] = 0; pop_ret[k] = POP_NONE; departed[k] = false;
            k++
        :: (k >= N_KEYS) -> break
        od
    };

    run BgWriter();
    run Evictor();
    run Remover();
#ifdef SSD_EVICTOR
    run SsdEvictor();
#endif

    k = 0;
    do
    :: (k < N_KEYS) -> run Populator(k); k++
    :: (k >= N_KEYS) -> break
    od;
    k = 0;
    do
    :: (k < N_LOOK) -> run Looker(); k++
    :: (k >= N_LOOK) -> break
    od;

    (pop_done == N_KEYS && look_done == N_LOOK);
    shutdown = true;
    timeout;

    /* P-FIN */
    d_step {
        k = 0;
        cnt = 0;
        do
        :: (k < N_KEYS) ->
            assert(read_refs[k] == 0 && !write_ref[k]);
            /* every memory-tier slot belongs to a committed MemoryTier entry */
#ifndef NO_SLOT_CHECK
            assert(!in_pool[k] || (committed[k] && location[k] == MT));
#endif
            /* Ok-cached and never removed => still resolvable */
            assert(pop_ret[k] != POP_OK_CACHED || committed[k] || departed[k]);
            if
            :: in_pool[k] -> cnt++
            :: else -> skip
            fi;
            k++
        :: (k >= N_KEYS) -> break
        od;
        assert(cnt == pool_used)
    }
}
