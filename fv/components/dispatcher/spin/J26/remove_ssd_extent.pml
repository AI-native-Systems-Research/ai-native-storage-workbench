/*
* remove_ssd_extent.pml — Component-level model: dispatcher remove() vs. the
* SSD extent lifecycle.
*
* Question modelled: when IDispatcher::remove(key) (dispatcher/src/lib.rs:3096)
* deletes an entry that is in the memory tier AND already has a copy on SSD
* (its write-through finished, so the entry has an ssd_offset), is the SSD
* extent always released?  Can a removed key leave an extent no entry owns
* (leak), or can an extent be released twice?
*
* Actors running concurrently (all spawned by init):
*   RemoveClient — client calling remove(key) twice  (lib.rs:3096-3136)
*   Populator    — client store path populate(key)   (lib.rs:3143-3199, 3489-3569)
*   Writer       — background write-through worker   (lib.rs:1390-1465; background.rs:117-143)
*   Demoter      — memory-tier demotion              (lib.rs:1050-1108; background.rs:471-575)
*   SsdEvictor   — background SSD evictor            (background.rs:279-370)
*   Lookup       — one client lookup/load            (lib.rs:1360-1387, 900-903)
*
* One cache key; one drive/extent-manager; N_EXT distinct SSD offsets.
* A generation counter separates entry instances created by re-population.
*
* Builds:
*   spin -a remove_ssd_extent.pml                          # BASE  (code as-is)
*   spin -DCONTROL -a ...                                  # minimal-fix control
*   spin -DCONTROL -DINJECT_STALE_OFFSET -a ...            # mutant (must fail)
*   -DDISABLE_P1 .. -DDISABLE_P5 isolate one property.
*   -DCHECK_DRAM_HAZARD adds an extra (out-of-scope-P1..P5) check of the
*     evictor's mt.remove-before-dm.remove window.
*/

#define NOOFF 255   /* sentinel: no SSD offset */
#define NOGEN 255   /* sentinel: lookup saw no entry (NotExist) */
#define ABORT 254   /* sentinel: dm.lookup wait abstracted as timeout */

#define N_EXT 2

mtype { MT, BD }    /* dispatch-map entry location (dispatch-map/src/entry.rs) */

/* ---- dispatch map: the single cached key (dispatch-map/src/lib.rs) ---- */
bool  entry_exists; /* entry present                                   (dm :334-357)  */
mtype loc;          /* MT = MemoryTier, BD = BlockDevice               (entry.rs)     */
bool  has_ssd;      /* MemoryTier.ssd_offset is Some                   (dm :184-186)  */
byte  ssd_off;      /* offset claimed by the entry (valid if BD or has_ssd)          */
byte  read_ref;     /* read pin count                                  (dm :147-150)  */
byte  write_ref;    /* write pin count (0/1)                           (dm :239-265)  */
byte  gen;          /* generation of the current entry instance (create: :393-439)  */

/* ---- memory tier ---- */
bool  mt_slot;      /* DRAM slot present for the key (imemory_tier.rs) */

/* ---- extent manager (extent-manager/src/lib.rs) ---- */
bool  ext_owned[N_EXT]; /* extent committed by publish()            (em :610)      */
bool  ext_resv[N_EXT];  /* extent reserved, WriteHandle outstanding (em :584-630)  */
byte  ext_serial[N_EXT];/* serial of the instance currently at each offset: an
                          * offset can be freed and reused by a later reserve, so
                          * extent identity is (offset, serial), not offset alone  */
byte  next_serial;

chan  jobq = [2] of { byte };  /* WriteJob channel (background.rs:50-51, unbounded in code) */

bool  no_more_jobs;
byte  actors_done;
byte  writer_done;

/* dm.release_read(key) — dispatch-map/src/lib.rs:267-285.
 * Key-based, NOT entry-based: it touches whatever entry exists now. */
inline dm_release_read()
{
    if
    :: entry_exists && read_ref > 0 -> read_ref--
    :: else -> skip   /* KeyNotFound / RefCountUnderflow; every caller ignores (let _) */
    fi
}

/* P3 helper: the entry's claimed extent is currently allocated. */
inline check_p3()
{
#ifdef DISABLE_P3
    skip
#else
    if
    :: entry_exists && (loc == BD || has_ssd) && !ext_owned[ssd_off] ->
        printf("P3 VIOLATION: entry claims offset %d that is not allocated\n", ssd_off);
        assert(0)
    :: else -> skip
    fi
#endif
}

/* dm.convert_to_storage(key, off) — dispatch-map/src/lib.rs:176-208. */
inline dm_convert_to_storage(off)
{
    if
    :: !entry_exists -> skip   /* KeyNotFound: extent stays committed -> orphan  */
    :: loc == BD -> skip       /* InvalidState, ignored by caller               */
    :: else ->
        has_ssd = true; ssd_off = off;   /* dm :184-186 (overwrites!)           */
        if
        :: read_ref > 0 -> read_ref--    /* dm :194-196, silently guarded       */
        :: else -> skip
        fi
    fi
    check_p3()
}

/* drive.extent_mgr.remove_extent(offset) — dispatcher lib.rs:3131, background.rs:348.
 * extent-manager lib.rs:680-684: unknown offset -> OffsetNotFound, ignored (let _). */
inline ext_release(off)
{
#ifndef DISABLE_P2
    if
    :: !ext_owned[off] ->
        printf("P2 VIOLATION: remove_extent(%d) on an extent that is not allocated (double release)\n", off);
        assert(0)
    :: else -> skip
    fi
#endif
    ext_owned[off] = false
    check_p3()
}

/* ------------------------------------------------------------------ */
/* Client calling IDispatcher::remove(key) — lib.rs:3096-3136.        */
/* ------------------------------------------------------------------ */
proctype RemoveClient()
{
    byte calls = 0;
    byte r_off;      /* offset learned from dm.lookup (BlockDevice arm only) */
    byte r_gen;      /* generation seen by this call's lookup                */
    bool r_bd;       /* lookup returned BlockDevice                          */
    bool r_removed;  /* dm.remove succeeded                                  */
    byte r_god;      /* offset the removed entry actually owned (god view)   */
    byte r_god_ser;  /* serial of the extent instance the removed entry owned */
    byte r_rel;      /* offset this call releases                             */

    do
    :: calls < 2 ->
        calls++;

        /* 1. dm.lookup(key) — lib.rs:3105 (dm lookup :128-174). The condvar
         *    wait on write_ref is abstracted as an immediate Timeout. */
        atomic {
        if
        :: !entry_exists ->
            r_gen = NOGEN; r_off = NOOFF; r_bd = false  /* NotExist: NO ref taken (dm :142-144) */
        :: write_ref == 1 ->
            r_gen = ABORT; r_off = NOOFF; r_bd = false  /* Timeout (dm :138-139) -> lib.rs:3114 */
        :: else ->
            r_gen = gen; read_ref++;                    /* dm :147-150 */
            if
            :: loc == BD -> r_off = ssd_off; r_bd = true  /* BlockDevice{offset} :3106-3109 */
            :: else -> r_off = NOOFF; r_bd = false        /* MemoryTier: ssd_offset NOT exposed */
            fi
        fi
        }

        if
        :: r_gen == ABORT -> skip   /* return Err(KeyNotFound) — lib.rs:3114, no release */
        :: else ->
            /* 2. dm.release_read(key) — lib.rs:3107/3111. BASE releases on BOTH Ok
             *    arms, including NotExist which took no ref. */
#ifdef CONTROL
            if
            :: r_gen == NOGEN -> skip   /* FIX (P5): NotExist took no pin; do not release */
            :: else ->
#endif
            atomic {
                if
                :: entry_exists && read_ref > 0 ->
                    read_ref--;
#ifndef DISABLE_P5
                    if
                    :: r_gen == gen -> skip   /* our own pin, same entry instance */
                    :: else ->
                        printf("P5 VIOLATION: remove released a pin it did not take (lookup saw gen %d, entry is gen %d)\n", r_gen, gen);
                        assert(0)
                    fi
#endif
                :: else -> skip   /* entry gone / underflow: ignored error */
                fi
            }
#ifdef CONTROL
            fi
#endif

            /* 3. dm.remove(key) — lib.rs:3119 (dm remove :334-357). */
            atomic {
            if
            :: !entry_exists -> r_removed = false                    /* -> Err :3120 */
            :: read_ref > 0 || write_ref > 0 -> r_removed = false    /* ActiveReferences */
            :: else ->
                r_removed = true;
                if
                :: loc == BD || has_ssd -> r_god = ssd_off           /* what the entry owns */
                :: else -> r_god = NOOFF
                fi
                if
                :: r_god != NOOFF -> r_god_ser = ext_serial[r_god]   /* instance identity   */
                :: else -> r_god_ser = 0
                fi
#ifndef CONTROL
                /* BASE: release the offset learned at dm.lookup time — NONE for a
                 * MemoryTier hit, because LookupResult::MemoryTier hides ssd_offset. */
                r_rel = r_off;
#else
                /* CONTROL FIX (P1/P2): the offset is read under the dm.remove lock. */
                r_rel = r_god;
#endif
#ifdef INJECT_STALE_OFFSET
                /* MUTANT (on top of CONTROL): regress to the lookup-time offset
                 * whenever lookup saw BlockDevice — the remove/evictor TOCTOU. */
                if
                :: r_bd -> r_rel = r_off
                :: else -> skip
                fi
#endif
                entry_exists = false; read_ref = 0; write_ref = 0;
                loc = MT; has_ssd = false
            fi
            }

            if
            :: !r_removed -> skip   /* return Err(KeyNotFound) */
            :: else ->
                /* 4. mt.remove(key) — lib.rs:3123-3125 (best effort, after dm.remove). */
                atomic { mt_slot = false }

                /* 5. drive.extent_mgr.remove_extent(offset) — lib.rs:3127-3133. */
                if
                :: r_rel != NOOFF -> atomic { ext_release(r_rel) }
                :: else -> skip
                fi

                /* return Ok — lib.rs:3135.
                 * P1: the extent instance the removed entry owned is released
                 * or was never allocated. (Identity = (offset, serial): a
                 * correct release may be followed by offset reuse.) */
#ifndef DISABLE_P1
                if
                :: r_god != NOOFF && ext_owned[r_god] && ext_serial[r_god] == r_god_ser ->
                    printf("P1 VIOLATION: remove returned Ok but the extent (offset %d, serial %d) of the removed entry is still allocated\n", r_god, r_god_ser);
                    assert(0)
                :: else -> skip
                fi
#endif
            fi
        fi
    :: else -> break
    od
    actors_done++
}

/* ------------------------------------------------------------------ */
/* Client store path: populate(key) — lib.rs:3143-3199 and            */
/* copy_gpu_to_memory_completed — lib.rs:3489-3569.                   */
/* ------------------------------------------------------------------ */
proctype Populator()
{
    bool ok = false;
    if
    :: true ->
        atomic {
        if
        :: !entry_exists ->
            /* reserve_memory + D2H + create_memory_tier_entry
             * (lib.rs:3159-3198; dm :393-439) */
            gen++;
            entry_exists = true; loc = MT; has_ssd = false; ssd_off = NOOFF;
            read_ref = 0; write_ref = 1;   /* dm :423 */
            mt_slot = true;
            ok = true
        :: else -> ok = false   /* AlreadyExists (dm :410-412): populate fails */
        fi
        }
        if
        :: ok ->
            /* The integrity-check section between create and downgrade
             * (lib.rs:3520-3550) is NOT modelled — see README "Left out". */
            atomic { write_ref = 0; read_ref++ }   /* downgrade_reference, lib.rs:3552 (dm :307-332: read_ref += 1) */
            atomic { jobq ! 0 }                        /* writer.enqueue, lib.rs:3559-3566 */
        :: else -> skip
        fi
    :: true -> skip   /* choose not to store */
    fi
    actors_done++
}

/* ------------------------------------------------------------------ */
/* Memory-tier demotion: evict_one_clean — lib.rs:1050-1108, and      */
/* MemoryTierEvictor — background.rs:471-575.                          */
/* ------------------------------------------------------------------ */
proctype Demoter()
{
    bool demoted = false;
    if
    :: true ->
        atomic {
        /* dm.try_evict_to_block — dm :548-580 */
        if
        :: entry_exists && loc == MT && has_ssd && read_ref == 0 && write_ref == 0 ->
            loc = BD;   /* keeps ssd_off: data stays resolvable on SSD */
            demoted = true
        :: else -> skip   /* KeyNotFound / ActiveReferences / InvalidState: skip */
        fi
        }
        if
        :: demoted -> atomic { mt_slot = false }   /* mt.remove — lib.rs:1068, background.rs:535 */
        :: else -> skip
        fi
    :: true -> skip
    fi
    actors_done++
}

/* ------------------------------------------------------------------ */
/* Background SSD evictor — background.rs:279-370 (one eviction).      */
/* ------------------------------------------------------------------ */
proctype SsdEvictor()
{
    byte e_off;
    bool e_removed = false;
    if
    :: true ->   /* utilization above threshold — abstracted nondeterministically (:296-304) */
        /* get_evictable_offset — background.rs:322, 384-404 (dm.lookup + release) */
        atomic {
        if
        :: !entry_exists -> e_off = NOOFF        /* NotExist / err -> skip */
        :: write_ref == 1 -> e_off = NOOFF       /* lookup wait abstracted as timeout */
        :: write_ref == 0 && entry_exists && loc == BD -> e_off = ssd_off; read_ref++   /* dm.lookup takes the ref (dm :147) */
        :: else -> e_off = NOOFF                 /* MemoryTier: hot, skip (:393-396) */
        fi
        }
        if
        :: e_off == NOOFF -> skip   /* continue */
        :: else ->
            atomic { dm_release_read() }   /* background.rs:390 */
            atomic {
                mt_slot = false;   /* mt.remove — background.rs:328, BEFORE dm.remove */
#ifdef CHECK_DRAM_HAZARD
                if
                :: entry_exists && loc == MT ->
                    printf("DRAM HAZARD: evictor freed the DRAM slot of a live MemoryTier entry (background.rs:328)\n");
                    assert(0)
                :: else -> skip
                fi
#endif
            }
            atomic {
            /* dm.remove — background.rs:331 */
            if
            :: !entry_exists -> e_removed = false
            :: read_ref > 0 || write_ref > 0 -> e_removed = false
            :: else ->
                e_removed = true;
#ifdef CONTROL
                /* FIX (P1/P2): offset read under the dm.remove lock. */
                if
                :: loc == BD || has_ssd -> e_off = ssd_off
                :: else -> e_off = NOOFF
                fi
#endif
                entry_exists = false; read_ref = 0; write_ref = 0;
                loc = MT; has_ssd = false
            fi
            }
            if
            :: e_removed && e_off != NOOFF -> atomic { ext_release(e_off) }   /* em.remove_extent — background.rs:348 */
            :: else -> skip
            fi
        fi
    :: true -> skip   /* utilization below threshold: no eviction this round */
    fi
    actors_done++
}

/* ------------------------------------------------------------------ */
/* One client lookup/load — warm path lib.rs:1360-1387,               */
/* cold-path promote lib.rs:900-903.                                   */
/* ------------------------------------------------------------------ */
proctype Lookup()
{
    byte l_kind;   /* 0=absent, 1=timeout, 2=MemoryTier, 3=BlockDevice */
    if
    :: true ->
        atomic {
        /* dm.lookup — dm :128-174. Guards must be mutually exclusive: when the
         * entry is absent, loc still holds its stale reset value MT. */
        if
        :: !entry_exists -> l_kind = 0
        :: write_ref == 1 -> l_kind = 1
        :: write_ref == 0 && entry_exists && loc == MT -> l_kind = 2; read_ref++
        :: else -> l_kind = 3; read_ref++
        fi
        }
        if
        :: l_kind == 0 || l_kind == 1 -> skip   /* KeyNotFound / timeout */
        :: l_kind == 2 ->
            skip   /* serve_memory_tier_to_gpu — lib.rs:1362-1363 */
            atomic { dm_release_read() }   /* lib.rs:1364 */
        :: l_kind == 3 ->
            /* cold path: allocate DRAM slot, read SSD, promote in place */
            atomic { mt_slot = true }   /* mt alloc (approximated) */
            skip   /* SSD -> DRAM read */
            atomic {
            /* dm.promote_block_to_memory_tier — dm :482-528 */
            if
            :: entry_exists && loc == BD -> loc = MT; has_ssd = true   /* retains ssd_off (dm :502-508) */
            :: else -> skip   /* KeyNotFound / InvalidState: load fails */
            fi
            }
            atomic { dm_release_read() }   /* serve + release — lib.rs:1364 */
        fi
    :: true -> skip   /* choose not to look up */
    fi
    actors_done++
}

/* ------------------------------------------------------------------ */
/* Background writer: process_write_job — lib.rs:1390-1465;           */
/* worker_loop — background.rs:117-143.                                */
/* ------------------------------------------------------------------ */
proctype Writer()
{
    byte w_off;
    bool w_ok;
    bool w_fail;
    do
    :: len(jobq) > 0 ->
        atomic { jobq ? _ }   /* recv_timeout Ok — background.rs:126-128 */

        atomic {
        /* mt.peek(job.key) — lib.rs:1399 */
        if
        :: mt_slot -> w_ok = true
        :: else -> w_ok = false   /* -> release_read, return (:1400-1404) */
        fi
        }
        if
        :: !w_ok -> atomic { dm_release_read() }
        :: else ->
            atomic {
            /* iem.reserve_extent — lib.rs:1439 (em :584-630) */
            if
            :: !ext_resv[0] && !ext_owned[0] -> ext_resv[0] = true; w_off = 0; w_ok = true
            :: !ext_resv[1] && !ext_owned[1] -> ext_resv[1] = true; w_off = 1; w_ok = true
            :: else -> w_ok = false   /* OutOfSpace -> release + abort (:1441-1444) */
            fi
            }
            if
            :: !w_ok -> atomic { dm_release_read() }
            :: else ->
                if
                :: true -> w_fail = true   /* write_buffer_to_ssd fails — lib.rs:1451-1456 */
                :: true -> w_fail = false
                fi
                if
                :: w_fail ->
                    atomic { ext_resv[w_off] = false }   /* WriteHandle drop -> abort (iextent_manager.rs:149-155) */
                    atomic { dm_release_read() }
                :: else ->
                    atomic {   /* write_handle.publish() — lib.rs:1463: commits a new extent instance */
                        ext_resv[w_off] = false; ext_owned[w_off] = true;
                        ext_serial[w_off] = next_serial; next_serial++
                    }
                    atomic { dm_convert_to_storage(w_off) }                        /* lib.rs:1464 */
                fi
            fi
        fi
        progress_job: skip   /* every dequeued job completes */
    :: no_more_jobs && len(jobq) == 0 -> break
    od
    writer_done = 1
}

/* ------------------------------------------------------------------ */
init
{
    /* Initial state: populate(key) has finished through downgrade_reference;
     * the write-through job is enqueued but not yet processed
     * (lib.rs:3198, 3552, 3561). This is the prompt's starting point. */
    gen = 0; entry_exists = true; loc = MT; has_ssd = false; ssd_off = NOOFF;
    read_ref = 1; write_ref = 0;   /* the enqueued job's pin */
    mt_slot = true;
    ext_owned[0] = false; ext_owned[1] = false;
    ext_resv[0] = false; ext_resv[1] = false;
    ext_serial[0] = 0; ext_serial[1] = 0; next_serial = 1;
    jobq ! 0;

    run Writer();
    run Populator();
    run RemoveClient();
    run Demoter();
    run SsdEvictor();
    run Lookup();

    (actors_done == 5);      /* all non-writer actors finished */
    no_more_jobs = true;     /* no further enqueues are possible */
    (writer_done == 1);      /* writer drained the queue */

    /* ---- Final invariants ---- */
#ifndef DISABLE_P4
    if
    :: ext_owned[0] && !(entry_exists && (loc == BD || has_ssd) && ssd_off == 0) ->
        printf("P4 VIOLATION: extent 0 is allocated but no entry owns it (leak)\n");
        assert(0)
    :: else -> skip
    fi
    if
    :: ext_owned[1] && !(entry_exists && (loc == BD || has_ssd) && ssd_off == 1) ->
        printf("P4 VIOLATION: extent 1 is allocated but no entry owns it (leak)\n");
        assert(0)
    :: else -> skip
    fi
    if
    :: ext_resv[0] || ext_resv[1] ->
        printf("P4 VIOLATION: dangling extent reservation at end\n");
        assert(0)
    :: else -> skip
    fi
    if
    :: len(jobq) > 0 ->
        printf("P4 VIOLATION: write job never processed\n");
        assert(0)
    :: else -> skip
    fi
#endif
    check_p3()   /* P3 at end: entry's claimed extent is allocated */
#ifndef DISABLE_P5
    if
    :: entry_exists && (read_ref > 0 || write_ref > 0) ->
        printf("P5 VIOLATION: pin left held at end (read_ref=%d, write_ref=%d)\n", read_ref, write_ref);
        assert(0)
    :: else -> skip
    fi
#endif
}
