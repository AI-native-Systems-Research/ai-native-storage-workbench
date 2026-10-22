/*
 * System-level model: clear_memory_tier vs a pinned lookup vs the background writer
 *
 * Written from scratch for node7 J22 (2026-10-18) from ai-native-storage-certus @ acd24025.
 * Method: fv/skills/tools-spin-model (workbench), incl. the "Added in the workbench" rules:
 * every value the modelled API returns is modelled here or listed under "Left out" in README.md.
 *
 * Question: clear_memory_tier (components/dispatcher/src/lib.rs:3794-3825) ignores the dispatch
 * map's refusal for a pinned entry (`let _ = dm.remove(key)`, lib.rs:3819). Does that leave a
 * dispatch-map entry pointing at a released memory-tier slot, or an entry still in the map?
 *
 * Component wiring modelled (as wired in certus-server):
 *   Clear     (IDispatcher::clear_memory_tier, lib.rs:3794)
 *               --> mt.oldest_keys (memory-tier lib.rs:435)
 *               --> dm.try_evict_to_block (dispatch-map lib.rs:548-580)
 *               --> mt.remove (memory-tier lib.rs:483) / dm.remove (dispatch-map lib.rs:334-357)
 *   Looker[a] (IDispatcher::lookup_async, lib.rs:2994-3070, NotExist / MemoryTier / BlockDevice arms)
 *               --> dm.lookup (dispatch-map lib.rs:128-174: takes a read pin on a hit)
 *               --> serve_memory_tier_to_gpu reads the slot (lib.rs:3048-3055)
 *               --> dm.release_read (dispatch-map lib.rs:267-285; called at lib.rs:3056 / 3064)
 *   BgWriter  (process_write_job, lib.rs:1390-1465)
 *               --> mt.peek (memory-tier lib.rs:423) ; write_buffer_to_ssd reads the slot (lib.rs:1451)
 *               --> dm.convert_to_storage (dispatch-map lib.rs:176-208: sets ssd_offset, drops 1 read ref)
 *               --> dm.release_read on every failure exit (lib.rs:1402, 1432, 1442, 1454)
 *
 * Start state = just after copy_gpu_to_memory_completed for every key (lib.rs:3489-3568):
 *   create_memory_tier_entry (lib.rs:3508; write_ref=1) -> downgrade_reference (lib.rs:3552;
 *   write_ref 1 -> read_ref 1, the pin that the write job owns) -> writer.enqueue (lib.rs:3561).
 *
 * Properties (assert-based; LTL only for termination):
 *   PA  (a) no dispatch-map MemoryTier entry points at a released slot     -- Monitor, every state
 *   PA1     a lookup never reads a released slot while serving             -- Looker
 *   PA2     the writer never reads a released slot while writing to SSD    -- BgWriter
 *   PB  (b) when clear_memory_tier returns, every entry is BlockDevice      -- Clear, at return
 *           (with its SSD offset) or is gone
 *   PC  (c) no pin is released by an actor that did not take it            -- release_read / convert_to_storage
 *   PD      clear_memory_tier's returned count == entries actually cleared -- Clear, at return
 *   PF      final: no leaked pins, no orphan memory-tier slot              -- init, at quiescence
 *   L1      clear_memory_tier eventually returns (LTL, weak fairness)      -- liveness run only
 *
 * Bounds: N_KEYS=2, N_LOOK=2 lookers (each one lookup of a nondeterministic key), 1 writer
 * (one job per key), 1 Clear call. Spin explores every interleaving of these at step granularity:
 * each dispatch-map / memory-tier call is one atomic step (each takes its component's lock).
 *
 * Build: -DFIX selects the minimal fix (see clear_memory_tier_control.pml).
 *        -DLIVE adds the termination claim (used by `make liveness`).
 */

#define N_KEYS   2
#define N_LOOK   2
#define W        0                 /* actor id of the background writer   */
#define N_ACT    (N_LOOK + 1)      /* actors that can hold a read pin      */
#define OWN(a,k) own[(a)*N_KEYS + (k)]

/* results returned by the modelled calls (Returned values table in README.md) */
mtype = { MT, BD,                                   /* dispatch-map Location                        */
          OK, E_KNF, E_ACTIVE, E_INVALID, E_UNDERFLOW,  /* DispatchMapError / MemoryTierError variants */
          R_MT, R_BD, R_NOTEXIST,                   /* LookupResult                                 */
          R_SOME, R_NONE,                           /* mt.peek                                      */
          L_OK, L_KNF, L_COLD };                    /* lookup_async result to its caller            */

/* ---------- dispatch map (dispatch-map lib.rs: Entry { location, read_ref, write_ref }) ---------- */
bool  dm_present[N_KEYS];
mtype dm_loc[N_KEYS];
bool  dm_has_off[N_KEYS];     /* Location::MemoryTier { ssd_offset: Some(_) }                 */
byte  read_ref[N_KEYS];
byte  own[N_ACT * N_KEYS];    /* ghost: read pins held, per actor (for PC)                     */

/* ---------- memory tier (memory-tier lib.rs: pool.slots) ---------- */
bool  mt_present[N_KEYS];     /* a slot is allocated for the key                               */

/* ---------- background writer queue (background.rs:51 unbounded; bounded here at N_KEYS) ---------- */
chan  wq = [N_KEYS] of { byte };

bool  clear_done = false;
byte  clear_count = 0;        /* the usize clear_memory_tier returns (lib.rs:3807, 3821, 3824) */
byte  done = 0;               /* finished worker processes                                    */
mtype look_ret[N_ACT];        /* lookup_async result per looker                               */

/* Property switches: all on by default; -DONLY_<P> keeps only that property (used to collect one
 * counterexample per property). A switched-off assert is `skip`, never a blocking expression. */
#if defined(ONLY_PA)||defined(ONLY_PA1)||defined(ONLY_PA2)||defined(ONLY_PB)||defined(ONLY_PC)||defined(ONLY_PD)||defined(ONLY_PF)
#define ONLY_SET 1
#endif
#if !defined(ONLY_SET) || defined(ONLY_PA)
#define CHK_PA(e)  assert(e)
#else
#define CHK_PA(e)  skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PA1)
#define CHK_PA1(e) assert(e)
#else
#define CHK_PA1(e) skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PA2)
#define CHK_PA2(e) assert(e)
#else
#define CHK_PA2(e) skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PB)
#define CHK_PB(e)  assert(e)
#else
#define CHK_PB(e)  skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PC)
#define CHK_PC(e)  assert(e)
#else
#define CHK_PC(e)  skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PD)
#define CHK_PD(e)  assert(e)
#else
#define CHK_PD(e)  skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PF)
#define CHK_PF(e)  assert(e)
#else
#define CHK_PF(e)  skip
#endif

#define SLOT_OK(k)   (!(dm_present[k] && dm_loc[k] == MT) || mt_present[k])
#define CLEARED(k)   (!dm_present[k] || dm_loc[k] == BD)

/* ---------------- dispatch-map calls: one atomic step each (one inner.lock() each) ---------------- */

/* dm.lookup: waits for write_ref==0 (none held after the start state), pins on a hit (lib.rs:128-174) */
inline dm_lookup(a, k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = R_NOTEXIST              /* lib.rs:142: NotExist, no pin taken */
        :: else ->
            read_ref[k]++; OWN(a, k)++;                   /* lib.rs:146-149 */
            if
            :: dm_loc[k] == MT -> r = R_MT                /* lib.rs:156-159 */
            :: else            -> r = R_BD                /* lib.rs:155 */
            fi
        fi
    }
}

/* dm.release_read (dispatch-map lib.rs:267-285) */
inline dm_release_read(a, k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                    /* lib.rs:272 */
        :: else ->
            if
            :: read_ref[k] == 0 -> r = E_UNDERFLOW        /* lib.rs:274-276 */
            :: else ->
                CHK_PC(OWN(a, k) > 0);                    /* PC: only the pin's taker releases it */
                read_ref[k]--; OWN(a, k)--;               /* lib.rs:278 */
                r = OK
            fi
        fi
    }
}

/* dm.convert_to_storage (dispatch-map lib.rs:176-208): sets ssd_offset, then drops ONE read ref
 * if any is held - whoever took it (lib.rs:194-196). */
inline dm_convert_to_storage(a, k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                    /* lib.rs:181 */
        :: dm_present[k] && dm_loc[k] == BD -> r = E_INVALID   /* lib.rs:187-191 */
        :: dm_present[k] && dm_loc[k] == MT ->
            dm_has_off[k] = true;                         /* lib.rs:184-185 */
            if
            :: read_ref[k] > 0 ->
                CHK_PC(OWN(a, k) > 0);                    /* PC */
                read_ref[k]--; OWN(a, k)--                /* lib.rs:194-196 */
            :: else -> skip
            fi;
            r = OK
        fi
    }
}

/* dm.try_evict_to_block (dispatch-map lib.rs:548-580) */
inline dm_try_evict(k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                    /* lib.rs:553 */
        :: else ->
            if
            :: read_ref[k] != 0 -> r = E_ACTIVE           /* lib.rs:558-560 (write_ref is 0 after start) */
            :: else ->
                if
                :: dm_loc[k] == MT && dm_has_off[k] ->
                    dm_loc[k] = BD; r = OK                /* lib.rs:563-570 */
                :: else -> r = E_INVALID                  /* lib.rs:571-578 */
                fi
            fi
        fi
    }
}

/* dm.remove (dispatch-map lib.rs:334-357) */
inline dm_remove(k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                    /* lib.rs:339 */
        :: else ->
            if
            :: read_ref[k] > 0 -> r = E_ACTIVE            /* lib.rs:341-343 */
            :: else ->
                dm_present[k] = false; dm_has_off[k] = false;   /* lib.rs:346 */
                r = OK
            fi
        fi
    }
}

/* ---------------- memory-tier calls ---------------- */

/* mt.remove (memory-tier lib.rs:483): frees the slot by key */
inline mt_remove(k, r) {
    atomic {
        if
        :: mt_present[k] -> mt_present[k] = false; r = OK
        :: else -> r = E_KNF
        fi
    }
}

/* mt.peek (memory-tier lib.rs:423-433) */
inline mt_peek(k, r) {
    atomic {
        if
        :: mt_present[k] -> r = R_SOME
        :: else -> r = R_NONE
        fi
    }
}

/* ---------------- PA monitor: checked in every reachable state ----------------
 * The usual Spin invariance pattern (manuals/invariance.html): the monitor waits, blocked, until
 * the invariant is false in some state; the end label makes "never fired" a valid end state. */
active proctype Monitor()
{
end:
    atomic { !(SLOT_OK(0) && SLOT_OK(1)) -> CHK_PA(false) }   /* PA */
}

/* ---------------- IDispatcher::clear_memory_tier (lib.rs:3794-3825) ---------------- */
proctype Clear()
{
    bool  cand[N_KEYS];
    byte  k;
    mtype r1, r2;

    do
    :: true ->
        /* let candidates = mt.oldest_keys(64)  (lib.rs:3809; one snapshot under the pool lock) */
        atomic { cand[0] = mt_present[0]; cand[1] = mt_present[1] };
        if
        :: !cand[0] && !cand[1] -> break                  /* lib.rs:3810-3812 */
        :: else -> skip
        fi;
        k = 0;
        do
        :: k < N_KEYS && cand[k] ->
            dm_try_evict(k, r1);                          /* lib.rs:3814 */
#ifndef FIX
            /* SHIPPED CODE (lib.rs:3814-3821) */
            if
            :: r1 == OK ->
                mt_remove(k, r2)                          /* lib.rs:3815: let _ = mt.remove(key) */
            :: else ->
                mt_remove(k, r2);                         /* lib.rs:3818: let _ = mt.remove(key) */
                dm_remove(k, r2)                          /* lib.rs:3819: let _ = dm.remove(key)  -- result ignored */
            fi;
            clear_count++;                                /* lib.rs:3821: count += 1 */
#else
            /* MINIMAL FIX: never free a slot the map may still point at; keep a pinned key for the next pass */
            if
            :: r1 == OK ->                                /* demoted: no reader can obtain the pointer any more */
                mt_remove(k, r2); clear_count++
            :: r1 == E_ACTIVE -> skip                     /* pinned: leave it; the outer loop retries */
            :: r1 == E_INVALID ->                         /* unpersisted: drop the map entry FIRST */
                dm_remove(k, r2);
                if
                :: r2 == OK -> mt_remove(k, r2); clear_count++
                :: else -> skip                           /* pinned in between: retry next pass */
                fi
            :: r1 == E_KNF ->                             /* orphan slot: no map entry */
                mt_remove(k, r2); clear_count++
            fi;
#endif
            k++
        :: k < N_KEYS && !cand[k] -> k++
        :: k >= N_KEYS -> break
        od
    od;

    /* Ok(count) returned (lib.rs:3824) */
    atomic {
        clear_done = true;
        CHK_PB(CLEARED(0) && CLEARED(1));                 /* PB */
        CHK_PD(clear_count == (!dm_present[0] || dm_loc[0] == BD) + (!dm_present[1] || dm_loc[1] == BD));  /* PD */
    }
    done++
}

/* ---------------- lookup_async with a pin (lib.rs:2994-3070) ---------------- */
proctype Looker(byte a)
{
    byte  k;
    mtype r, r2;

    if :: k = 0 :: k = 1 fi;
    dm_lookup(a, k, r);                                   /* lib.rs:3011 */
    if
    :: r == R_NOTEXIST -> look_ret[a] = L_KNF             /* lib.rs:3029 */
    :: r == R_MT ->
        CHK_PA1(mt_present[k]);                           /* PA1: copy starts from a live slot */
        skip;                                             /* serve_memory_tier_to_gpu (lib.rs:3048-3055): H2D window */
        CHK_PA1(mt_present[k]);                           /* PA1: still live when the copy completes */
        dm_release_read(a, k, r2);                        /* lib.rs:3056 */
        look_ret[a] = L_OK
    :: r == R_BD ->
        dm_release_read(a, k, r2);                        /* lib.rs:3064 */
        look_ret[a] = L_COLD                              /* promote_and_serve (lib.rs:3065): Left out */
    fi;
    done++
}

/* ---------------- process_write_job (lib.rs:1390-1465), one job per key ---------------- */
proctype BgWriter()
{
    byte  k, jobs = 0;
    mtype r, r2;

    do
    :: jobs < N_KEYS ->
        wq ? k; jobs++;                                   /* worker_loop recv (background.rs:126) */
        mt_peek(k, r);                                    /* lib.rs:1399 */
        if
        :: r == R_NONE ->
            dm_release_read(W, k, r2)                     /* lib.rs:1400-1404 */
        :: r == R_SOME ->
            if
            :: true ->                                    /* DmaBuffer ok, reserve_extent ok, write ok */
                CHK_PA2(mt_present[k]);                   /* PA2: the SSD write starts from a live slot */
                skip;                                     /* write_buffer_to_ssd (lib.rs:1451) */
                CHK_PA2(mt_present[k]);                   /* PA2: still live when the write completes */
                dm_convert_to_storage(W, k, r2)           /* lib.rs:1463-1464 (publish: Left out) */
            :: true ->                                    /* DmaBuffer::from_raw / reserve_extent / write failed */
                dm_release_read(W, k, r2)                 /* lib.rs:1432 / 1442 / 1454 */
            fi
        fi
    :: jobs >= N_KEYS -> break
    od;
    done++
}

#ifdef LIVE
ltl L1 { <> clear_done }                                  /* clear_memory_tier returns */
#endif

init
{
    byte k;
    d_step {
        k = 0;
        do
        :: k < N_KEYS ->
            /* state right after copy_gpu_to_memory_completed (lib.rs:3508, 3552, 3561) */
            mt_present[k] = true;
            dm_present[k] = true; dm_loc[k] = MT; dm_has_off[k] = false;
            read_ref[k] = 1; OWN(W, k) = 1;               /* the write job owns the downgraded ref */
            wq ! k;
            k++
        :: k >= N_KEYS -> break
        od
    };
    atomic {
        run Clear();
        run BgWriter();
        run Looker(1);
        run Looker(2)
    };
    (done == 4);

    /* PF: quiescent final state */
    d_step {
        k = 0;
        do
        :: k < N_KEYS ->
            CHK_PF(read_ref[k] == 0 || !dm_present[k]);   /* PF: no leaked pins */
            CHK_PF(OWN(W, k) == 0 && OWN(1, k) == 0 && OWN(2, k) == 0);   /* PF */
            CHK_PF(!mt_present[k] || (dm_present[k] && dm_loc[k] == MT));   /* PF: no orphan slot */
            k++
        :: k >= N_KEYS -> break
        od
    }
}
