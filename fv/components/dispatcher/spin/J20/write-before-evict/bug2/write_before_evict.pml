/*
 * Certus Dispatcher: Write-Before-Evict Safety Model (v4)
 *
 * J20 sync to origin/unstable @ acd24025. All file:line below are at that
 * commit; paths relative to components/.
 *
 * Verifies that memory-tier eviction never produces a dangling reference:
 * no dispatch-map entry resolves to SSD data that was never written, and no
 * dispatch-map entry or in-flight populate uses a DRAM slot that was freed.
 *
 * Key modeling decisions matching the real code:
 *   - populate (dispatcher/src/lib.rs:3143-3207) = reserve_memory
 *     (lib.rs:3318-3420) -> evict_and_insert (lib.rs:1176-1224) ->
 *     evict_for_space (lib.rs:1123-1168) -> evict_one_clean (lib.rs:1050-1108);
 *     then copy_gpu_to_memory_async: mt.get (lib.rs:3435) + D2H
 *     (lib.rs:3192-3194); then copy_gpu_to_memory_completed: mt.get
 *     (lib.rs:3506), dm.create_memory_tier_entry (lib.rs:3508-3518),
 *     dm.downgrade_reference (lib.rs:3552), bg_writer.enqueue (lib.rs:3561).
 *     mt.insert and dm.create_memory_tier_entry are SEPARATE steps: in
 *     between, the key owns a DRAM slot but has no dispatch-map entry.
 *   - evict_one_clean ONLY demotes: dm.try_evict_to_block (lib.rs:1066;
 *     dispatch-map/src/lib.rs:548-580) then mt.remove (lib.rs:1068). Pinned
 *     (ActiveReferences, lib.rs:1073-1075) and all other refusals
 *     (Err(_), lib.rs:1076-1083) are SKIPPED. No blind free, no dm.remove
 *     (spec dispatcher 001 FR-024 l.301, edge case l.234). The v3 blind-LRU
 *     path and its LOST / EVICTED_PENDING states no longer exist in the code.
 *   - Nothing evictable -> AllocationFailed (lib.rs:1157-1163, 1193-1206) ->
 *     bounded backpressure retry (lib.rs:3401-3416) -> best-effort drop:
 *     populate returns Ok(()) without caching (lib.rs:3161-3174, FR-060).
 *   - Background writer process_write_job (lib.rs:1390-1465): mt.peek by KEY
 *     (lib.rs:1399); every failure path calls dm.release_read; success calls
 *     dm.convert_to_storage (lib.rs:1464), which sets ssd_offset AND drops the
 *     read ref (dispatch-map/src/lib.rs:176-208). The writer channel is
 *     UNBOUNDED (background.rs:51-52): enqueue never blocks.
 *   - SSD evictor evictor_loop (background.rs:279-370): for a BlockDevice
 *     entry (get_evictable_offset, background.rs:384-404) it calls
 *     mt.remove(key) (background.rs:328) BEFORE dm.remove(key)
 *     (background.rs:331). mt.remove is keyed (memory-tier/src/lib.rs:483-514),
 *     it does not check WHICH slot it frees.
 *
 * key_gen[k] is no longer a writer guard (the code has none: mt.peek checks
 * presence by key only, lib.rs:1399). It now models DRAM-slot identity: the
 * *mut u8 that mt.insert returns (memory-tier/src/lib.rs:328-390) and that
 * create_memory_tier_entry stores in the dispatch-map. Each mt.insert of key k
 * yields a fresh generation, so dm_ptr[k] != mt_slot[k] means "resolves to a
 * freed slot".
 *
 * Safety properties verified:
 *   P1: ssd_offset is set only by the writer, after it wrote THIS entry's slot.
 *   P2: BlockDevice (ON_SSD) state requires ssd_offset (no dangling SSD ref).
 *   P3: No dispatch-map MemoryTier entry resolves to a freed DRAM slot.
 *       (v3: "LOST -> !in_dispatch_map". Same intent: a DRAM slot gone without
 *       SSD backing must not stay resolvable. LOST itself is legacy.)
 *   P4: dm.remove / dm.try_evict_to_block never succeed while refs are held;
 *       at quiescence every write-through pin has been released.
 *   P5: A DRAM slot is never freed while in use: while the dispatch-map
 *       resolves to it, or while a populate holds its pointer for the D2H
 *       copy (FR-024: the DM transition happens BEFORE the slot is freed).
 *   P6: populate returns only specified outcomes (FR-003, FR-060, US1 AS3):
 *       Ok (cached), Ok (dropped on full), AlreadyExists. Never
 *       AllocationFailed or KeyNotFound; downgrade_reference never fails.
 *   P7: spec dispatcher 002 FR-033: evictions_blocked_unpersisted is recorded
 *       only when try_evict_to_block refused for want of an ssd_offset.
 *
 * Parameters (README "Tuning"): N_CLIENTS=2, N_KEYS=3, POOL_CAP=2,
 *   QUEUE_CAP=4 (= total populates, so the unbounded channel never blocks),
 *   MAX_ATTEMPTS=2, BP_RETRIES=1.
 *
 * Control switches (none defined = today's code; see *_control.pml):
 *   FIX_SSD_EVICTOR  remove the mt.remove at background.rs:328
 *   FIX_FR033_LABEL  count only InvalidState(no ssd_offset) as unpersisted
 */

/* ---------- Parameters ---------- */
#define N_CLIENTS     2
#define N_KEYS        3
#define POOL_CAP      2
#define QUEUE_CAP     4
#define MAX_ATTEMPTS  2
#define BP_RETRIES    1

/* ---------- Dispatch-map location (dispatch-map/src/entry.rs Location) ---------- */
mtype = { DM_NONE, DM_MT, DM_BD };

/* ---------- populate() return values ---------- */
mtype = { R_NONE, R_OK_CACHED, R_OK_DROPPED, R_ALREADY_EXISTS,
          R_KEY_NOT_FOUND, R_ALLOC_FAILED };

/* ---------- try_evict_to_block() results (dispatch-map/src/lib.rs:548-580) ---------- */
mtype = { TEV_OK, TEV_ACTIVE_REFS, TEV_NO_OFFSET, TEV_NOT_MT, TEV_KEY_NOT_FOUND };

/* ---------- Dispatch-map per-key state ---------- */
mtype dm_loc[N_KEYS];
byte  dm_ptr[N_KEYS];      /* slot generation a MemoryTier entry points at */
bool  dm_ssd[N_KEYS];      /* ssd_offset is Some (MT) / offset valid (BD)  */
byte  dm_rref[N_KEYS];
bool  dm_wref[N_KEYS];

/* ---------- Memory-tier per-key state ---------- */
byte  mt_slot[N_KEYS];     /* 0 = no slot, else slot generation            */
byte  key_gen[N_KEYS];     /* last generation handed out by mt.insert      */
byte  pool_used = 0;

/* Ghost: slot generation a populate is copying into (D2H), 0 = none. */
byte  filling[N_KEYS];

/* ---------- Write job channel (unbounded crossbeam channel) ---------- */
chan write_queue = [QUEUE_CAP] of { byte };

/* ---------- Coordination ---------- */
byte clients_done = 0;
bool shutdown = false;

/* ---------- mt.remove(key) (memory-tier/src/lib.rs:483-514) ---------- */
/* Keyed: frees WHATEVER slot key x holds now. P5 checked at every call. */
inline mt_remove(x)
{
    atomic {
        if
        :: (mt_slot[x] != 0) ->
            /* P5: never free a slot the dispatch-map still resolves to,
             * or a slot a populate is copying into. */
            assert(!(dm_loc[x] == DM_MT && dm_ptr[x] == mt_slot[x]));
            assert(filling[x] != mt_slot[x]);
            mt_slot[x] = 0;
            pool_used--
        :: else -> skip   /* Err(KeyNotFound), ignored by every caller */
        fi
    }
}

/* ---------- evict_one_clean (lib.rs:1050-1108) ---------- */
/* Scans mt.oldest_keys (lib.rs:1056); index order stands for LRU order. */
inline evict_one_clean()
{
    freed = false;
    c = 0;
    do
    :: (c < N_KEYS && !freed) ->
        if
        :: (mt_slot[c] != 0) ->
            /* dm.try_evict_to_block(cand) (lib.rs:1066), one DM lock hold */
            atomic {
                if
                :: (dm_loc[c] == DM_NONE) ->
                    tev = TEV_KEY_NOT_FOUND
                :: (dm_loc[c] != DM_NONE && (dm_rref[c] > 0 || dm_wref[c])) ->
                    tev = TEV_ACTIVE_REFS
                :: (dm_loc[c] == DM_MT && dm_rref[c] == 0 && !dm_wref[c]
                    && dm_ssd[c]) ->
                    /* P2: demotion to BlockDevice requires ssd_offset. */
                    assert(dm_ssd[c]);
                    /* P4: never succeed while refs are held. */
                    assert(dm_rref[c] == 0 && !dm_wref[c]);
                    dm_loc[c] = DM_BD;
                    tev = TEV_OK
                :: (dm_loc[c] == DM_MT && dm_rref[c] == 0 && !dm_wref[c]
                    && !dm_ssd[c]) ->
                    tev = TEV_NO_OFFSET
                :: (dm_loc[c] == DM_BD && dm_rref[c] == 0 && !dm_wref[c]) ->
                    tev = TEV_NOT_MT
                fi
            };
            if
            :: (tev == TEV_OK) ->
                /* let _ = mt.remove(cand) (lib.rs:1068) */
                mt_remove(c);
                freed = true            /* return true (lib.rs:1071) */
            :: (tev == TEV_ACTIVE_REFS) ->
                /* record_eviction_blocked_by_pin (lib.rs:1073-1075) */
                skip
            :: else ->
#ifdef FIX_FR033_LABEL
                /* fixed: only InvalidState(no ssd_offset) is "unpersisted" */
                counted_unpersisted = (tev == TEV_NO_OFFSET);
#else
                /* Err(_) => record_eviction_blocked_unpersisted (lib.rs:1076-1083) */
                counted_unpersisted = true;
#endif
                /* P7 (spec 002 FR-033): evictions_blocked_unpersisted is
                 * recorded only for a missing ssd_offset. */
                assert(!counted_unpersisted || tev == TEV_NO_OFFSET)
            fi
        :: else -> skip
        fi;
        c++
    :: else -> break
    od
    /* !freed: record_eviction_scan_exhausted; return false (lib.rs:1106-1107) */
}

/* ---------- Client process: populate(key) (lib.rs:3143-3207) ---------- */
proctype Client(byte client_id)
{
    byte my_key;
    byte i = 0;
    byte my_gen;
    byte ptr;
    byte attempts;
    byte bp;
    byte c;
    bool freed;
    bool counted_unpersisted;
    bool reserved;
    mtype tev;
    mtype ret;

    do
    :: (i < 2) ->
        /* Client 0 populates keys 0,1; client 1 populates keys 2,0
         * (key 0 is stored twice: duplicate and re-populate paths). */
        my_key = (client_id == 0 -> i : (2 - 2 * i));
        i++;
        ret = R_NONE;
        reserved = false;
        attempts = 0;
        bp = 0;

        /* ---- Phase 1: reserve_memory (lib.rs:3159) -> evict_and_insert ---- */
        do
        :: (ret == R_NONE && !reserved) ->
            if
            :: (pool_used >= POOL_CAP) ->
                /* evict_for_space loop (lib.rs:1134-1165) */
                if
                :: (attempts >= MAX_ATTEMPTS) ->
                    ret = R_ALLOC_FAILED        /* lib.rs:1136-1152 */
                :: else ->
                    attempts++;
                    evict_one_clean();
                    if
                    :: !freed -> ret = R_ALLOC_FAILED   /* lib.rs:1157-1163 */
                    :: else -> skip
                    fi
                fi
            :: else ->
                /* mt.insert(key, size) (lib.rs:1188; memory-tier/src/lib.rs:328-390) */
                atomic {
                    if
                    :: (mt_slot[my_key] != 0) ->
                        /* AlreadyExists (memory-tier :356-358) -> lib.rs:1208-1217
                         * -> reserve_memory Err(e) (:3417) -> populate Err(e) (:3176) */
                        ret = R_ALREADY_EXISTS
                    :: (mt_slot[my_key] == 0 && pool_used < POOL_CAP) ->
                        key_gen[my_key]++;
                        mt_slot[my_key] = key_gen[my_key];
                        my_gen = key_gen[my_key];
                        pool_used++;
                        reserved = true
                    :: (mt_slot[my_key] == 0 && pool_used >= POOL_CAP) ->
                        /* PoolFull (lib.rs:1190-1191) */
                        attempts++
                    fi
                };
                if
                :: (!reserved && ret == R_NONE && attempts >= MAX_ATTEMPTS) ->
                    ret = R_ALLOC_FAILED        /* lib.rs:1192-1196 */
                :: else -> skip
                fi
            fi;
            /* Backpressure: retry AllocationFailed until the deadline
             * (lib.rs:3401-3416), then best-effort drop (lib.rs:3161-3174). */
            if
            :: (ret == R_ALLOC_FAILED && bp < BP_RETRIES) ->
                bp++;
                attempts = 0;
                ret = R_NONE
            :: (ret == R_ALLOC_FAILED && bp >= BP_RETRIES) ->
                ret = R_OK_DROPPED              /* return Ok(()) (lib.rs:3174) */
            :: else -> skip
            fi
        :: else -> break
        od;

        if
        :: reserved ->
            /* ---- Phase 2: copy_gpu_to_memory_async, mt.get (lib.rs:3435) ---- */
            atomic {
                if
                :: (mt_slot[my_key] == 0) ->
                    ret = R_KEY_NOT_FOUND       /* ok_or(KeyNotFound) (lib.rs:3435) */
                :: else ->
                    /* P5: the slot populate copies into is the one it reserved. */
                    assert(mt_slot[my_key] == my_gen);
                    filling[my_key] = mt_slot[my_key]
                fi
            };
            if
            :: (ret == R_NONE) ->
                /* D2H memcpy + stream_synchronize (lib.rs:3192-3194):
                 * the slot must still be ours when the copy lands (P5). */
                atomic {
                    assert(mt_slot[my_key] == my_gen);
                    filling[my_key] = 0
                };
                /* ---- Phase 3: copy_gpu_to_memory_completed (lib.rs:3198) ---- */
                /* mt.get(key) (lib.rs:3506) */
                atomic {
                    ptr = mt_slot[my_key];
                    if
                    :: (ptr == 0) -> ret = R_KEY_NOT_FOUND   /* lib.rs:3506 */
                    :: else -> filling[my_key] = ptr
                    fi
                };
                if
                :: (ret == R_NONE) ->
                    /* dm.create_memory_tier_entry (lib.rs:3508;
                     * dispatch-map/src/lib.rs:393-439) */
                    atomic {
                        filling[my_key] = 0;
                        if
                        :: (dm_loc[my_key] != DM_NONE) ->
                            ret = R_ALREADY_EXISTS      /* lib.rs:3510-3513 */
                        :: else ->
                            dm_loc[my_key] = DM_MT;
                            dm_ptr[my_key] = ptr;
                            dm_ssd[my_key] = false;
                            dm_rref[my_key] = 0;
                            dm_wref[my_key] = true;
                            /* P3: the entry just published resolves to a live slot. */
                            assert(mt_slot[my_key] == dm_ptr[my_key])
                        fi
                    };
                    if
                    :: (ret == R_ALREADY_EXISTS) ->
                        mt_remove(my_key)               /* lib.rs:3511 */
                    :: else ->
                        /* dm.downgrade_reference (lib.rs:3552;
                         * dispatch-map/src/lib.rs:307-332) */
                        atomic {
                            /* P6: cannot fail — we hold the write ref, so the
                             * entry cannot be removed or demoted meanwhile. */
                            assert(dm_loc[my_key] != DM_NONE && dm_wref[my_key]);
                            dm_wref[my_key] = false;
                            dm_rref[my_key]++
                        };
                        /* bg_writer.enqueue (lib.rs:3559-3566); unbounded */
                        write_queue ! my_key;
                        ret = R_OK_CACHED               /* Ok(()) (lib.rs:3206) */
                    fi
                :: else -> skip
                fi
            :: else -> skip
            fi
        :: else -> skip
        fi;

        /* P6: populate returns only specified outcomes. */
        assert(ret == R_OK_CACHED || ret == R_OK_DROPPED || ret == R_ALREADY_EXISTS)

    :: (i >= 2) ->
        break
    od;

    clients_done++
}

/* ---------- Background Writer: process_write_job (lib.rs:1390-1465) ---------- */
proctype BgWriter()
{
    byte job_key;
    byte wslot;

    do
    :: atomic { write_queue ? job_key ->
        /* mt.peek(job.key) (lib.rs:1399) — keyed, no LRU refresh.
         * Merged with the receive: the channel is unbounded, so the receive
         * frees no capacity anyone waits on and moving it later is invisible. */
        wslot = mt_slot[job_key] };
        if
        :: (wslot == 0) ->
            /* None => release_read; return (lib.rs:1400-1404).
             * P3: the job's pin keeps the entry MemoryTier; if its slot is
             * gone, the dispatch-map resolves to a freed slot. */
            atomic {
                assert(dm_loc[job_key] != DM_MT);
                assert(dm_loc[job_key] != DM_NONE && dm_rref[job_key] > 0);
                dm_rref[job_key]--
            }
        :: else ->
            /* DmaBuffer::from_raw (lib.rs:1421-1435) cannot fail here: ptr
             * non-null, aligned_bytes > 0 (interfaces/src/spdk_types.rs:299-308).
             * See README "Left out". */
            if
            :: true ->
                /* reserve_extent Err (SSD full) => release_read (lib.rs:1439-1444)
                 * OR write_buffer_to_ssd Err => release_read (lib.rs:1451-1456).
                 * Identical effect (WriteHandle drop aborts the extent,
                 * interfaces/src/iextent_manager.rs:149-155), one branch. */
                atomic {
                    assert(dm_loc[job_key] != DM_NONE && dm_rref[job_key] > 0);
                    dm_rref[job_key]--
                }
            :: true ->
                /* publish (lib.rs:1463) + convert_to_storage (lib.rs:1464;
                 * dispatch-map/src/lib.rs:176-208): sets ssd_offset, drops
                 * the read ref. */
                atomic {
                    /* P4: Ok expected — the pin keeps the entry MemoryTier. */
                    assert(dm_loc[job_key] == DM_MT);
                    /* P1: the slot we wrote is the entry's own slot. */
                    assert(wslot == dm_ptr[job_key]);
                    /* P5: and it was not freed while the SSD write read it. */
                    assert(mt_slot[job_key] == wslot);
                    dm_ssd[job_key] = true;
                    if
                    :: (dm_rref[job_key] > 0) -> dm_rref[job_key]--
                    :: else -> skip
                    fi
                }
            fi
        fi

    :: shutdown && empty(write_queue) ->
        break
    od
}

/* ---------- Background SSD Evictor: evictor_loop (background.rs:279-370) ---------- */
/* "utilization >= threshold" (background.rs:296-305) abstracted: may run any time. */
proctype SsdEvictor()
{
    byte victim;
    bool is_bd;

    do
    :: !shutdown ->
        atomic {
            /* a candidate from dm.oldest_keys (background.rs:314); local choice */
            if
            :: victim = 0
            :: victim = 1
            :: victim = 2
            fi;
            /* get_evictable_offset (background.rs:322, 384-404): dm.lookup takes
             * and release_read drops a read ref — net effect: the location only.
             * A held write ref makes lookup time out -> Err -> None. */
            is_bd = (dm_loc[victim] == DM_BD && !dm_wref[victim])
        };
        if
        :: is_bd ->
#ifndef FIX_SSD_EVICTOR
            /* let _ = mt.remove(key) "no-op if not present" (background.rs:327-328) */
            mt_remove(victim);
#endif
            /* dm.remove(key) (background.rs:331; dispatch-map/src/lib.rs:334-357) */
            atomic {
                if
                :: (dm_loc[victim] != DM_NONE && dm_rref[victim] == 0 && !dm_wref[victim]) ->
                    /* P4: dm.remove never succeeds while refs are held. */
                    assert(dm_rref[victim] == 0 && !dm_wref[victim]);
                    /* P2: a BlockDevice entry had its data on SSD. */
                    assert(dm_loc[victim] != DM_BD || dm_ssd[victim]);
                    dm_loc[victim] = DM_NONE;
                    dm_ssd[victim] = false;
                    dm_ptr[victim] = 0
                    /* em.remove_extent(offset) (background.rs:345-349) */
                :: else -> skip   /* is_err() => continue (background.rs:331-333) */
                fi
            }
        :: else -> skip
        fi
    :: shutdown ->
        break
    od
}

/* ---------- Initialization ---------- */
init
{
    byte k;
    byte n;

    d_step {
        k = 0;
        do
        :: (k < N_KEYS) ->
            dm_loc[k] = DM_NONE;
            dm_ptr[k] = 0;
            dm_ssd[k] = false;
            dm_rref[k] = 0;
            dm_wref[k] = false;
            mt_slot[k] = 0;
            key_gen[k] = 0;
            filling[k] = 0;
            k++
        :: (k >= N_KEYS) ->
            break
        od
    };

    atomic {
        run BgWriter();
        run SsdEvictor();
        run Client(0);
        run Client(1)
    };

    /* Wait for all clients to complete. */
    (clients_done == N_CLIENTS);

    /* Signal shutdown. */
    shutdown = true;

    /* Wait for background processes to exit. */
    timeout;

    /* Final invariant checks. */
    d_step {
        k = 0;
        n = 0;
        do
        :: (k < N_KEYS) ->
            /* P2: BlockDevice -> ssd_offset */
            assert(dm_loc[k] != DM_BD || dm_ssd[k]);
            /* P3: a MemoryTier entry resolves to its live slot */
            assert(dm_loc[k] != DM_MT || mt_slot[k] == dm_ptr[k]);
            /* P4: every write-through pin was released */
            assert(dm_rref[k] == 0 && !dm_wref[k]);
            if
            :: (mt_slot[k] != 0) -> n++
            :: else -> skip
            fi;
            k++
        :: (k >= N_KEYS) ->
            break
        od;
        /* pool accounting */
        assert(n == pool_used)
    }
}
