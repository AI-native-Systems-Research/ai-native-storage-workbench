/*
 * System-level model: populate -> write-through (success AND failure) -> flush_to_ssd -> clear_memory_tier
 *
 * Written from scratch for node7 J23 (2026-10-19) from ai-native-storage-certus @ af221dd82
 * (current unstable, holds the 7 merged fixes of PR #555). Method: workbench fv/skills/tools-spin-model,
 * incl. the "Added in the workbench" rules: every value the modelled API returns is modelled here or
 * listed under "Left out" in README.md; every file:line is re-derived from af221dd82.
 *
 * Question (dispatcher root cause #5): flush_to_ssd (components/dispatcher/src/lib.rs:3894-3907) only
 * waits until the write-through queue is empty (background.rs:198-205 -> 99-103). A job that fails inside
 * process_write_job (lib.rs:1395-1470: extent reservation 1444-1449, SSD write 1456-1461) leaves the entry
 * MemoryTier with no SSD copy, and the queue still drains. clear_memory_tier (lib.rs:3861-3892) then
 * force-removes that entry (3885-3886): the only copy of data the caller flushed is gone.
 *
 * Contract (quoted in README.md):
 *   interfaces/src/idispatcher.rs:824-832 (flush_to_ssd doc): "Guarantees that every entry populated
 *     before this call has its data persisted to the block device. After this returns, clear_memory_tier
 *     will convert all memory-tier entries to BlockDevice state rather than dropping them. Returns the
 *     number of entries that now have a valid SSD offset."
 *   dispatcher spec FR-056 (spec.md:344): flush_to_ssd "returns the number of entries flushed".
 *   dispatcher spec FR-017 (spec.md:296): a failed write-through is silently dropped (known limitation).
 *   dispatcher spec FR-038 (spec.md:317): clear removes an entry whose transition fails.
 *
 * Actors (all concurrent):
 *   Populator[k]  IDispatcher::populate (lib.rs:3178-3259) for key k: reserve_memory (3194) ->
 *                 D2H + sync (3235-3246) -> copy_gpu_to_memory_completed (3250 -> 3541-3636):
 *                 create_memory_tier_entry (3560), downgrade_reference (3619), writer.enqueue (3628).
 *   BgWriter      BackgroundWriter::worker_loop (background.rs:117-143) -> process_write_job:
 *                 success -> convert_to_storage (lib.rs:1469); failure (DmaBuffer 1436-1439,
 *                 reserve_extent 1446-1449, SSD write 1458-1461) -> release_read only; in_flight-- (bg 129).
 *   Client        flush_to_ssd (lib.rs:3894) then clear_memory_tier (lib.rs:3861).
 *
 * Properties:
 *   PA (a) when flush_to_ssd returns Ok, every entry stored before the flush has an SSD copy
 *          (the interface-doc guarantee; README explains why this, not "or reports failure", is what the
 *          specification requires today)
 *   PB (b) clear_memory_tier never removes the only copy of a stored entry
 *   PC (c) flush_to_ssd's return value == number of entries with a valid SSD offset (interface doc)
 *   PF     final: no leaked pins, no orphan slot
 *   L1     the client eventually finishes (flush returns and clear returns) -- liveness, weak fairness
 *
 * Bounds: N_KEYS=2 keys, one populate each, both populated before the flush starts (as in "stored before
 * the flush"); 1 writer; 1 client. Each dispatch-map / memory-tier / queue call is one atomic step.
 * Build: -DFIX = minimal fix (see flush_clear_control.pml); -DLIVE adds L1; -DONLY_<P> keeps one property.
 */

#define N_KEYS 2

mtype = { MT, BD,
          OK, E_KNF, E_ACTIVE, E_INVALID, E_UNDERFLOW, E_IO,   /* dispatch-map / dispatcher errors */
          R_SOME, R_NONE };

/* ---- property switches (as in J22): -DONLY_<P> keeps only that property ---- */
#if defined(ONLY_PA)||defined(ONLY_PB)||defined(ONLY_PC)||defined(ONLY_PF)
#define ONLY_SET 1
#endif
#if !defined(ONLY_SET) || defined(ONLY_PA)
#define CHK_PA(e) assert(e)
#else
#define CHK_PA(e) skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PB)
#define CHK_PB(e) assert(e)
#else
#define CHK_PB(e) skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PC)
#define CHK_PC(e) assert(e)
#else
#define CHK_PC(e) skip
#endif
#if !defined(ONLY_SET) || defined(ONLY_PF)
#define CHK_PF(e) assert(e)
#else
#define CHK_PF(e) skip
#endif

/* ---------- dispatch map (dispatch-map lib.rs) ---------- */
bool  dm_present[N_KEYS];
mtype dm_loc[N_KEYS];
bool  dm_has_off[N_KEYS];      /* MemoryTier { ssd_offset: Some(_) } */
byte  read_ref[N_KEYS];
byte  write_ref[N_KEYS];

/* ---------- memory tier (memory-tier lib.rs) ---------- */
bool  mt_present[N_KEYS];

/* ---------- SSD (extent manager + block device), ghost: does a durable copy of k exist? ---------- */
bool  on_ssd[N_KEYS];

/* ---------- background writer (background.rs:51 unbounded channel; in_flight counter 79-80, 129) ---------- */
chan  wq = [N_KEYS] of { byte };
byte  in_flight = 0;

/* ---------- observation ---------- */
bool  stored[N_KEYS];          /* populate returned Ok with the entry cached (ghost) */
byte  pop_done = 0;
bool  flushed = false;         /* flush_to_ssd has returned Ok */
byte  flush_ret;               /* the usize it returned */
bool  client_done = false;
#ifdef FIX
bool  wt_failed[N_KEYS];       /* FIX: the writer records a failed job (see control) */
#endif

/* ================= dispatch-map calls (one atomic step each: one inner.lock()) ================= */

inline dm_release_read(k, r) {
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                         /* dispatch-map 272 */
        :: dm_present[k] && read_ref[k] == 0 -> r = E_UNDERFLOW  /* 274-276 */
        :: dm_present[k] && read_ref[k] > 0 -> read_ref[k]--; r = OK   /* 278 */
        fi
    }
}

inline dm_convert_to_storage(k, r) {                           /* dispatch-map 176-208 */
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF                         /* 181 */
        :: dm_present[k] && dm_loc[k] == BD -> r = E_INVALID   /* 187-191 */
        :: dm_present[k] && dm_loc[k] == MT ->
            dm_has_off[k] = true;                              /* 184-185 */
            if :: read_ref[k] > 0 -> read_ref[k]-- :: else -> skip fi;   /* 194-196 */
            r = OK
        fi
    }
}

inline dm_try_evict(k, r) {                                    /* dispatch-map 548-580 */
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF
        :: dm_present[k] && (read_ref[k] != 0 || write_ref[k] != 0) -> r = E_ACTIVE
        :: dm_present[k] && read_ref[k] == 0 && write_ref[k] == 0 ->
            if
            :: dm_loc[k] == MT && dm_has_off[k] -> dm_loc[k] = BD; r = OK
            :: else -> r = E_INVALID
            fi
        fi
    }
}

inline dm_remove(k, r) {                                       /* dispatch-map 334-357 */
    atomic {
        if
        :: !dm_present[k] -> r = E_KNF
        :: dm_present[k] && (read_ref[k] > 0 || write_ref[k] > 0) -> r = E_ACTIVE
        :: dm_present[k] && read_ref[k] == 0 && write_ref[k] == 0 ->
            /* PB: removing a stored entry whose data is only in DRAM destroys the only copy */
            CHK_PB(!stored[k] || on_ssd[k]);
            dm_present[k] = false; dm_has_off[k] = false; r = OK
        fi
    }
}

inline mt_remove(k, r) {                                       /* memory-tier 483 */
    atomic {
        if
        :: mt_present[k] ->
            /* PB: freeing the DRAM slot of a stored entry with no SSD copy, while the map still points at
             * it or has just dropped it, destroys the only copy */
            CHK_PB(!stored[k] || on_ssd[k]);
            mt_present[k] = false; r = OK
        :: else -> r = E_KNF
        fi
    }
}

/* ================= IDispatcher::populate, one key (lib.rs:3178-3259) ================= */
proctype Populator(byte k)
{
    /* reserve_memory -> evict_and_insert -> mt.insert (lib.rs:3194); room is assumed (pool >= N_KEYS) */
    atomic { mt_present[k] = true };
    /* D2H copy + stream_synchronize (lib.rs:3235-3246): success modelled; failure paths Left out (X3) */
    skip;
    /* copy_gpu_to_memory_completed (lib.rs:3541-3636) */
    atomic { dm_present[k] = true; dm_loc[k] = MT; dm_has_off[k] = false; write_ref[k] = 1 };   /* 3560 */
    atomic { write_ref[k] = 0; read_ref[k] = 1 };              /* downgrade_reference (3619) */
    atomic { in_flight++; wq ! k };                            /* writer.enqueue (3628 -> bg 79-81) */
    stored[k] = true;                                          /* populate returns Ok(()) (3258) */
    pop_done++
}

/* ================= BackgroundWriter::worker_loop + process_write_job ================= */
proctype BgWriter()
{
    byte  k, jobs = 0;
    mtype r;
    bool  peek_ok;

    do
    :: jobs < N_KEYS ->
        wq ? k; jobs++;                                        /* recv_timeout (bg 126) */
        atomic { peek_ok = mt_present[k] };                    /* mt.peek (lib.rs:1404) */
        if
        :: !peek_ok -> dm_release_read(k, r)                   /* 1406-1408 */
        :: peek_ok ->
            if
            :: true ->                                         /* reserve_extent + write_buffer_to_ssd + publish ok */
                atomic { on_ssd[k] = true };                   /* data durable on SSD (1456, 1468) */
                dm_convert_to_storage(k, r)                    /* 1469 */
            :: true ->                                         /* DmaBuffer (1436) / reserve_extent (1446) /
                                                                  SSD write (1458) failed: FR-017 silent drop */
#ifdef FIX
                wt_failed[k] = true;
#endif
                dm_release_read(k, r)                          /* 1437 / 1447 / 1459 */
            fi
        fi;
        atomic { in_flight-- }                                 /* bg 129, AFTER process_job returns */
    :: jobs >= N_KEYS -> break
    od
}

/* ================= client: flush_to_ssd then clear_memory_tier ================= */
proctype Client()
{
    byte  k, before, count, nk, got;
    bool  cand[N_KEYS];
    mtype r1, r2;
    bool  flush_err = false;

    (pop_done == N_KEYS);                                      /* both entries stored before the flush */

    /* ---- flush_to_ssd (lib.rs:3894-3907) ---- */
    atomic { before = in_flight };                             /* writer.in_flight() (3899; bg 193-195) */
    (in_flight == 0);                                          /* writer.flush() (3900; bg 198-205) */
#ifdef FIX
    /* FIX: flush reports a failed write-through instead of claiming durability */
    atomic { flush_err = wt_failed[0] || wt_failed[1] };
    if
    :: flush_err -> skip                                       /* Err(IoError(..)) returned */
    :: else ->
        atomic {
            nk = 0; k = 0;
            do :: k < N_KEYS -> if :: dm_present[k] && (dm_loc[k] == BD || dm_has_off[k]) -> nk++ :: else -> skip fi; k++
               :: k >= N_KEYS -> break od;
            flush_ret = nk                                     /* entries with a valid SSD offset */
        }
    fi;
#else
    flush_ret = before;                                        /* Ok(flushed) = the in_flight sampled BEFORE (3906) */
#endif
    if
    :: !flush_err ->
        atomic {
            flushed = true;
            /* PA: every entry stored before the flush is now on SSD */
            CHK_PA((!stored[0] || on_ssd[0]) && (!stored[1] || on_ssd[1]));
            /* PC: the count = entries that now have a valid SSD offset (interface doc 831) */
            nk = 0; k = 0;
            do :: k < N_KEYS -> if :: dm_present[k] && (dm_loc[k] == BD || dm_has_off[k]) -> nk++ :: else -> skip fi; k++
               :: k >= N_KEYS -> break od;
            CHK_PC(flush_ret == nk)
        }
    :: flush_err -> skip
    fi;

    /* ---- clear_memory_tier (lib.rs:3861-3892), shipped body ---- */
#ifdef FIX
    /* FIX: a caller told that the flush failed does not clear (it must not destroy the only copy);
       the shipped clear body below is otherwise unchanged */
    if :: flush_err -> goto finish :: else -> skip fi;
#endif
    count = 0;
    do
    :: true ->
        atomic { cand[0] = mt_present[0]; cand[1] = mt_present[1] };   /* mt.oldest_keys(64) (3876) */
        if :: !cand[0] && !cand[1] -> break :: else -> skip fi;        /* 3877-3879 */
        k = 0;
        do
        :: k < N_KEYS && cand[k] ->
            dm_try_evict(k, r1);                               /* 3881 */
            if
            :: r1 == OK -> mt_remove(k, r2)                    /* 3882: let _ = mt.remove(key) */
            :: else ->
                mt_remove(k, r2);                              /* 3885: let _ = mt.remove(key) */
                dm_remove(k, r2)                               /* 3886: let _ = dm.remove(key) */
            fi;
            count++;                                           /* 3888 */
            k++
        :: k < N_KEYS && !cand[k] -> k++
        :: k >= N_KEYS -> break
        od
    od;
#ifdef FIX
finish:
#endif
    client_done = true                                         /* Ok(count) (3891) */
}

#ifdef LIVE
ltl L1 { <> client_done }
#endif

init
{
    byte k;
    atomic {
        run Populator(0);
        run Populator(1);
        run BgWriter();
        run Client()
    };
    (client_done && in_flight == 0 && len(wq) == 0);
    d_step {
        k = 0;
        do
        :: k < N_KEYS ->
            CHK_PF(!dm_present[k] || (read_ref[k] == 0 && write_ref[k] == 0));   /* PF: no leaked pin */
            CHK_PF(!mt_present[k] || (dm_present[k] && dm_loc[k] == MT));        /* PF: no orphan slot */
            k++
        :: k >= N_KEYS -> break
        od
    }
}
