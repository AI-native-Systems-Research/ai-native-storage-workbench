//! Refutation harnesses, one per work-list property. Naming follows scorer_kani.py:
//! `refute_<id>` PASSES when it demonstrates a reachable violation of the property
//! from the stated start state. Each binds the outcome, `kani::cover!`s the violating
//! case and then asserts it on every path.
//!
//! Start states are post-`initialize` states of the real component (see
//! `Ctx::new`), plus the concurrent / device behaviour each harness names. The
//! declared code assumptions are stated with `kani::assume` where the property's
//! operation is one they cover.

use interfaces::{
    DispatcherError, EvictionEvent, EvictionReason, GpuStream, IpcHandle,
    ServedBy,
};

use crate::mirror::*;
use crate::model::*;

fn gpu_dst() -> *mut u8 {
    Box::leak(Box::new([0u8; 8])).as_mut_ptr()
}

fn handle(size: u32) -> IpcHandle {
    IpcHandle { address: gpu_dst(), size }
}

/// The warm stream `device_streams_for` creates first for a fresh device.
const WARM: usize = STREAM_BASE + 1;

fn pipe_streams() -> [GpuStream; 2] {
    [
        GpuStream((STREAM_BASE + 3) as *mut std::ffi::c_void),
        GpuStream((STREAM_BASE + 4) as *mut std::ffi::c_void),
    ]
}

/// Two cold-read jobs of one 512-byte segment each (LBA 0 and LBA 8).
fn two_jobs() -> [ColdReadJob; 2] {
    [
        ColdReadJob { mem_ptr: gpu_dst(), gpu_dst: gpu_dst() as *mut _, start_lba: 0, total_bytes: 512 },
        ColdReadJob { mem_ptr: gpu_dst(), gpu_dst: gpu_dst() as *mut _, start_lba: 8, total_bytes: 512 },
    ]
}

/// D-RANGE-FR-019-319d26 (segment_io) and D-RANGE-FR-019-73b5ad (cold read) for a
/// drive with 512-byte blocks and a 4 KiB max transfer, read in 4 KiB chunks.
fn assume_fr019(max_transfer_size: u32, sector_size: u32, chunk_size: usize) {
    kani::assume(max_transfer_size % sector_size == 0);
    kani::assume(max_transfer_size as usize >= chunk_size);
}

// ---------------------------------------------------------------------------
// batch_lookup
// ---------------------------------------------------------------------------

/// DIS-BATCH-LOOKUP-COLD-PROMOTES. Memory-tier-only mode, one block-device key
/// (symbolic key, 4-byte block). `batch_lookup` (lib.rs:2481-2507) inserts a slot and promotes the entry and
/// reports it served (`Ok`, `Ssd`), but nothing is read into the slot and no copy of
/// any kind (`memcpy_h2d_async` or `dma_copy_to_device`) reaches the GPU destination.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::serve_concurrently_promoted, crate::mirror::not_taken::serve_concurrently_promoted)]
#[kani::stub(crate::mirror::serve_memory_tier_to_gpu, crate::mirror::not_taken::serve_memory_tier_to_gpu)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
fn refute_dis_batch_lookup_cold_promotes() {
    let clock = Clock::new();
    let key: u64 = kani::any();
    let size: u32 = 4;
    let c = Ctx::new(
        Dm::new(&clock, [Some(Entry::block(key, 0)), None]),
        Mt::new(&clock, 16),
        Gpu::new(-1),
    );
    let entries = [(key, vec![handle(size)])];

    let out = batch_lookup(&c, &entries);

    let served = out[0].result.is_ok() && out[0].served_by == ServedBy::Ssd;
    let g = c.gpu.s.borrow();
    let copies = g.h2d_async + g.dma_to_device;
    kani::cover!(served && copies == 0);
    assert!(served, "the block-device entry is reported served");
    assert!(copies == 0, "but nothing was delivered to its GPU destination");
}

/// DIS-BATCH-LOOKUP-ERROR-ISOLATION. Two cold entries of a batch on one drive, one
/// 512-byte segment each. The drive reports the first entry's read timed out
/// (`Completion::Timeout`) and completes the second entry's read successfully.
/// `scatter_gather_multi_drive_zero_copy` (pipeline.rs:1050-1063) marks EVERY entry
/// on that drive failed, and batch_lookup reports each job's result as that entry's
/// result (lib.rs:2676-2713, `Err(e) => Err(e.clone())`): the second entry fails
/// although its own read succeeded.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_batch_lookup_error_isolation() {
    assume_fr019(4096, 512, 4096);
    let gpu = Gpu::new(0);
    let jobs = two_jobs();
    let chan = Chan::new([Cpl::Timeout, read_ok(1), Cpl::Closed, Cpl::Closed]);

    let results = scatter_gather_one_drive(512, &gpu, &pipe_streams(), &chan, &jobs, 4096, 128);

    let second_read_ok = chan.received.get() == 2;
    kani::cover!(results[0].is_err() && results[1].is_err() && second_read_ok);
    assert!(results[0].is_err(), "entry 0's read failed");
    assert!(second_read_ok, "entry 1's own read completed successfully");
    assert!(results[1].is_err(), "yet entry 1 is reported failed too");
}

/// DIS-BATCH-LOOKUP-ONE-WARM-SYNC-AFTER-COLD. A mixed batch: warm key A (resident)
/// and cold key B whose promotion a sibling lookup (the other TP rank) won the
/// `mt.insert` race for and finishes right after our classification. With a known
/// device, the batched warm sync runs once (lib.rs:2745-2750), and the
/// concurrent-promotion recovery serves B with `synchronize = true`
/// (lib.rs:1363, 1300-1303) — a second synchronization of the same warm stream.
#[kani::proof]
#[kani::unwind(3)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
fn refute_dis_batch_lookup_one_warm_sync_after_cold() {
    let clock = Clock::new();
    let (ka, kb) = (1u64, 2u64);
    let mt = Mt::new(&clock, 16).with_slot(ka, 4);
    let pool = mt.pool_ptr();
    mt.s.borrow_mut().resident_elsewhere = Some(kb);
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, 4, Some(0))), Some(Entry::block(kb, 8))]);
    dm.s.borrow_mut().flip = Some(Flip { key: kb, after: 1, pointer: pool, size: 4 });
    let c = Ctx::new(dm, mt, Gpu::new(0));
    let entries = [(ka, vec![handle(4)]), (kb, vec![handle(4)])];

    let out = batch_lookup(&c, &entries);

    let warm_syncs = c.gpu.s.borrow().syncs_on[0];
    let both_served = out[0].result.is_ok() && out[1].result.is_ok();
    kani::cover!(both_served && warm_syncs == 2);
    assert!(both_served);
    assert!(warm_syncs == 2, "the warm stream is synchronized twice in one batch");
}

/// DIS-BATCH-LOOKUP-SERVEDBY-CONCURRENT-DRAM. Cold key B; a sibling lookup won the
/// `mt.insert` race and finishes its SSD->DRAM read right after our classification.
/// The recovery pass (lib.rs:2941-2946) waits for the MemoryTier transition and
/// serves B from the memory tier, but the attribution stays the `Ssd` recorded at
/// classification (lib.rs:2435).
#[kani::proof]
#[kani::unwind(3)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
fn refute_dis_batch_lookup_servedby_concurrent_dram() {
    let clock = Clock::new();
    let kb: u64 = kani::any();
    let mt = Mt::new(&clock, 16);
    let pool = mt.pool_ptr();
    mt.s.borrow_mut().resident_elsewhere = Some(kb);
    let dm = Dm::new(&clock, [Some(Entry::block(kb, 8)), None]);
    dm.s.borrow_mut().flip = Some(Flip { key: kb, after: 1, pointer: pool, size: 4 });
    let c = Ctx::new(dm, mt, Gpu::new(-1));
    let entries = [(kb, vec![handle(4)])];

    let out = batch_lookup(&c, &entries);

    // Served from the memory-tier slot by the recovery pass: one DRAM->GPU copy.
    let from_dram = out[0].result.is_ok() && c.gpu.s.borrow().dma_to_device == 1;
    kani::cover!(from_dram && out[0].served_by == ServedBy::Ssd);
    assert!(from_dram, "B was served from the memory tier after waiting");
    assert!(out[0].served_by == ServedBy::Ssd, "but is attributed SSD, not DRAM");
}

/// DIS-BATCH-LOOKUP-SERVEDBY-OVERWRITE. Same race as above, with a known device: the
/// recovery pass rewrites `results[i]` from `AlreadyExists` to the DRAM serve's `Ok`
/// (lib.rs:2942-2944) without rewriting `tier[i]`, so the returned attribution is the
/// one of the pass that did NOT produce the final result.
#[kani::proof]
#[kani::unwind(3)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
fn refute_dis_batch_lookup_servedby_overwrite() {
    let clock = Clock::new();
    let kb: u64 = kani::any();
    let mt = Mt::new(&clock, 16);
    let pool = mt.pool_ptr();
    mt.s.borrow_mut().resident_elsewhere = Some(kb);
    let dm = Dm::new(&clock, [Some(Entry::block(kb, 8)), None]);
    dm.s.borrow_mut().flip = Some(Flip { key: kb, after: 1, pointer: pool, size: 4 });
    let c = Ctx::new(dm, mt, Gpu::new(0));
    let entries = [(kb, vec![handle(4)])];

    let out = batch_lookup(&c, &entries);

    // The final result came from the DRAM recovery pass: a warm-stream copy.
    let g = c.gpu.s.borrow();
    let rewritten_by_dram = out[0].result.is_ok() && g.h2d_async == 1 && g.last_h2d_stream == WARM;
    kani::cover!(rewritten_by_dram && out[0].served_by != ServedBy::Dram);
    assert!(rewritten_by_dram);
    assert!(out[0].served_by != ServedBy::Dram, "result rewritten, attribution not");
}

/// DIS-BATCH-LOOKUP-SYNC-FAILURE-ERROR. One warm hit with a known device: its copy is
/// queued on the warm stream and the batched `stream_synchronize` fails (a CUDA
/// error). The failure is only logged (lib.rs:2745-2750): the entry keeps `Ok` and
/// `Dram`, i.e. it is reported as a hit.
#[kani::proof]
#[kani::unwind(3)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::serve_concurrently_promoted, crate::mirror::not_taken::serve_concurrently_promoted)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
#[kani::stub(crate::mirror::cold_no_drives, crate::mirror::not_taken::cold_no_drives)]
fn refute_dis_batch_lookup_sync_failure_error() {
    let clock = Clock::new();
    let ka: u64 = kani::any();
    let size: u32 = 4;
    let mt = Mt::new(&clock, 16).with_slot(ka, size);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, size, Some(0))), None]);
    let gpu = Gpu::new(0);
    gpu.s.borrow_mut().fail_sync = true;
    let c = Ctx::new(dm, mt, gpu);
    let entries = [(ka, vec![handle(size)])];

    let out = batch_lookup(&c, &entries);

    let in_flight_then_failed = {
        let g = c.gpu.s.borrow();
        g.h2d_async == 1 && g.syncs == 1
    };
    let hit = out[0].result.is_ok() && out[0].served_by == ServedBy::Dram;
    kani::cover!(in_flight_then_failed && hit);
    assert!(in_flight_then_failed, "the copy was in flight and the sync failed");
    assert!(hit, "yet the entry is reported as a DRAM hit, not Error");
}

/// DIS-BATCH-LOOKUP-TRANSIENT-RETRY. A resident key whose dispatch-map entry a
/// concurrent writer keeps `write_ref`-held across the initial lookup and its five
/// retries (each `lookup` waits and returns `Err(Timeout)`, dispatch-map
/// lib.rs:121-130). After the retries, batch_lookup reports the transient error as
/// `KeyNotFound` (lib.rs:2445-2447), attributed `Miss` (lib.rs:2968). (With a remote
/// lookup bound, that `KeyNotFound` is what selects the key for forwarding,
/// lib.rs:2758-2765; the refutation needs only the first half of the property.)
#[kani::proof]
#[kani::unwind(7)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::serve_concurrently_promoted, crate::mirror::not_taken::serve_concurrently_promoted)]
#[kani::stub(crate::mirror::serve_memory_tier_to_gpu, crate::mirror::not_taken::serve_memory_tier_to_gpu)]
#[kani::stub(crate::mirror::cold_no_drives, crate::mirror::not_taken::cold_no_drives)]
#[kani::stub(crate::mirror::remote_pass, crate::mirror::not_taken::remote_pass)]
fn refute_dis_batch_lookup_transient_retry() {
    let clock = Clock::new();
    let ka: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(ka, 4);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, 4, Some(0))), None]);
    dm.s.borrow_mut().lookup_timeouts = 6;
    let c = Ctx::new(dm, mt, Gpu::new(-1));
    let entries = [(ka, vec![handle(4)])];

    let out = batch_lookup(&c, &entries);

    let retried = c.dm.s.borrow().lookups == 6;
    let still_present = c.dm.get(ka).is_some();
    let not_found = matches!(out[0].result, Err(DispatcherError::KeyNotFound(_)))
        && out[0].served_by == ServedBy::Miss;
    kani::cover!(retried && still_present && not_found);
    assert!(retried, "the lookup was retried five times");
    assert!(still_present, "the key is present in the dispatch map");
    assert!(not_found, "yet the transient error is reported KeyNotFound / Miss");
}

/// DIS-TIER-STATS-ROUTE-PARTITION. One key served by `lookup` from the memory tier.
/// `lookup_async` (lib.rs:3036-3062) counts it in `promotions_to_gpu` but none of
/// `lookup_hits_dram`, `lookup_hits_ssd`, `remote_lookup_hits` moves: the served key
/// is missing from the route partition (REAL `TierEventCounters`).
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::promote_and_serve, crate::mirror::not_taken::promote_and_serve)]
fn refute_dis_tier_stats_route_partition() {
    let clock = Clock::new();
    let ka: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(ka, 4);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, 4, Some(0))), None]);
    let c = Ctx::new(dm, mt, Gpu::new(-1));
    let before = c.tier_counters.snapshot();

    let r = lookup(&c, ka, handle(4));

    let after = c.tier_counters.snapshot();
    let served = r.is_ok() && after.promotions_to_gpu == before.promotions_to_gpu + 1;
    let routed = (after.lookup_hits_dram + after.lookup_hits_ssd + after.remote_lookup_hits)
        - (before.lookup_hits_dram + before.lookup_hits_ssd + before.remote_lookup_hits);
    kani::cover!(served && routed == 0);
    assert!(served, "one key served");
    assert!(routed == 0, "zero keys added to the route partition");
}

// ---------------------------------------------------------------------------
// lookup / lookup_async
// ---------------------------------------------------------------------------

/// DIS-LOOKUP-NO-HW-PROMOTE. Memory-tier-only mode, a block-device key. `lookup` ->
/// `promote_and_serve` (lib.rs:795-819) creates the slot and copies it to the GPU, but
/// re-registers with `create_memory_tier_entry` on a key that already exists, which
/// the dispatch map refuses with `AlreadyExists` (dispatch-map lib.rs:390-392); the
/// refusal is ignored and the entry stays in block-device state.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::serve_memory_tier_to_gpu, crate::mirror::not_taken::serve_memory_tier_to_gpu)]
fn refute_dis_lookup_no_hw_promote() {
    let clock = Clock::new();
    let key: u64 = kani::any();
    let offset: u64 = kani::any();
    let size: u32 = 4;
    let c = Ctx::new(
        Dm::new(&clock, [Some(Entry::block(key, offset)), None]),
        Mt::new(&clock, 16),
        Gpu::new(-1),
    );

    let r = lookup(&c, key, handle(size));

    let slot_and_copy = c.mt.peek(key).is_some() && c.gpu.s.borrow().dma_to_device == 1;
    let still_block = matches!(c.dm.get(key), Some(Entry { loc: Loc::Block { .. }, .. }));
    kani::cover!(r.is_ok() && slot_and_copy && still_block);
    assert!(r.is_ok() && slot_and_copy, "slot created and copied to the GPU");
    assert!(still_block, "but the entry is not re-registered as a memory-tier entry");
}

/// DIS-LOOKUP-ASYNC-WARM-STREAM. A memory-tier hit with a known device: the copy is
/// queued on that device's warm stream, and `lookup_async` synchronizes it itself and
/// returns the NULL stream (lib.rs:3048-3061), not the stream the copy was queued on.
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::promote_and_serve, crate::mirror::not_taken::promote_and_serve)]
fn refute_dis_lookup_async_warm_stream() {
    let clock = Clock::new();
    let ka: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(ka, 4);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, 4, Some(0))), None]);
    let c = Ctx::new(dm, mt, Gpu::new(0));

    let r = lookup_async(&c, ka, handle(4));

    let queued_on_warm = {
        let g = c.gpu.s.borrow();
        g.h2d_async == 1 && g.last_h2d_stream == WARM
    };
    let returned_null = matches!(r, Ok(s) if s.0.is_null());
    kani::cover!(queued_on_warm && returned_null);
    assert!(queued_on_warm, "the copy was queued on the warm stream");
    assert!(returned_null, "but lookup_async returns the null stream");
}

/// DIS-LOOKUP-ASYNC-DEVICE-STREAM. The dispatcher was initialized with a block-device
/// factory, so `initialize` allocated the shared warm stream (lib.rs:1938-1940). The
/// destination's device is unknown (`device_of_ptr` -> -1). `lookup_async` does not
/// fall back to the shared warm stream: `warm_raw` is 0 (lib.rs:3022-3023) and the
/// copy goes through the synchronous `dma_copy_to_device` (lib.rs:1306-1334).
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(crate::mirror::evict_one_clean, crate::mirror::not_taken::evict_one_clean)]
#[kani::stub(crate::mirror::promote_and_serve, crate::mirror::not_taken::promote_and_serve)]
fn refute_dis_lookup_async_device_stream() {
    let clock = Clock::new();
    let ka: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(ka, 4);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(ka, pool, 4, Some(0))), None]);
    let c = Ctx::new(dm, mt, Gpu::new(-1));
    c.initialized.set(false);
    initialize(&c, false, true).unwrap();
    let shared_warm = c.warm_stream.get();

    let r = lookup_async(&c, ka, handle(4));

    let g = c.gpu.s.borrow();
    let not_on_shared = g.h2d_async == 0 && g.dma_to_device == 1;
    kani::cover!(shared_warm != 0 && r.is_ok() && not_on_shared);
    assert!(shared_warm != 0, "a shared warm stream exists");
    assert!(r.is_ok());
    assert!(not_on_shared, "unknown device, yet the copy is not issued on it");
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

/// DIS-REMOVE-NO-WAIT. A memory-tier entry whose background write-through is still
/// pending: `populate` handed the job the read ref it downgraded to
/// (`downgrade_reference`), released only by the job's `convert_to_storage`. `remove`
/// (lib.rs:3119-3120) does not proceed: `dm.remove` refuses with
/// `ActiveReferences` and `remove` returns `KeyNotFound`, leaving the entry in place.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_remove_no_wait() {
    let clock = Clock::new();
    let k: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(k, 4);
    let pool = mt.pool_ptr();
    let mut e = Entry::mem(k, pool, 4, None);
    e.read_ref = 1;
    let mut c = Ctx::new(Dm::new(&clock, [Some(e), None]), mt, Gpu::new(-1));
    c.bg_writer_in_flight = Some(std::cell::Cell::new(1));

    let r = remove(&c, k);

    let refused = matches!(r, Err(DispatcherError::KeyNotFound(_)));
    let still_there = c.dm.get(k).is_some() && c.mt.peek(k).is_some();
    kani::cover!(refused && still_there);
    assert!(refused, "remove fails instead of proceeding");
    assert!(still_there, "and nothing was removed");
}

/// DIS-REMOVE-ORDER. A resident, unpinned memory-tier entry. `remove` deletes the
/// dispatch-map entry (lib.rs:3119) BEFORE it frees the memory-tier slot
/// (lib.rs:3123-3125).
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_remove_order() {
    let clock = Clock::new();
    let k: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(k, 4);
    let pool = mt.pool_ptr();
    let off: Option<u64> = kani::any();
    let c = Ctx::new(Dm::new(&clock, [Some(Entry::mem(k, pool, 4, off)), None]), mt, Gpu::new(-1));

    let r = remove(&c, k);

    let t_map = c.dm.s.borrow().t_remove_ok;
    let t_slot = c.mt.s.borrow().t_remove;
    let both = r.is_ok() && t_map != 0 && t_slot != 0;
    kani::cover!(both && t_map < t_slot);
    assert!(both, "both were removed");
    assert!(t_map < t_slot, "the dispatch-map entry went first, the slot second");
}

/// DIS-REMOVE-RESOURCES-REUSABLE. One data drive; a memory-tier entry whose
/// write-through landed (extent allocated at `off`, `ssd_offset = Some(off)`).
/// `remove` succeeds, but frees an SSD extent only for an entry in BlockDevice state
/// (lib.rs:3105-3115, 3127-3133): the extent stays allocated and is not reusable.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_remove_resources_reusable() {
    let clock = Clock::new();
    let k: u64 = kani::any();
    let off: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(k, 4);
    let pool = mt.pool_ptr();
    let c = Ctx::new(Dm::new(&clock, [Some(Entry::mem(k, pool, 4, Some(off))), None]), mt, Gpu::new(-1));
    c.data_drives.borrow_mut().push(Drive {
        extents: Extents::with(off),
        rw_stats: interfaces::ReadWriteStats::default(),
    });

    let r = remove(&c, k);

    let slot_freed = c.mt.peek(k).is_none();
    let extent_kept = c.data_drives.borrow()[0].extents.is_allocated(off);
    kani::cover!(r.is_ok() && slot_freed && extent_kept);
    assert!(r.is_ok() && slot_freed, "remove succeeded and freed the DRAM slot");
    assert!(extent_kept, "but the SSD extent is still allocated");
}

// ---------------------------------------------------------------------------
// flush_to_ssd / clear_memory_tier
// ---------------------------------------------------------------------------

/// DIS-FLUSH-THEN-CLEAR-DEMOTES. One data drive whose extent manager is full. A
/// stored entry's write-through job is in flight; the writer runs it, its
/// `reserve_extent` fails and the job ends leaving the entry unpersisted
/// (lib.rs:1436-1443). `flush_to_ssd` then returns (queue drained), and
/// `clear_memory_tier` cannot demote the entry (`try_evict_to_block` ->
/// `InvalidState`) and force-removes it (lib.rs:3814-3820): dropped, not demoted.
#[kani::proof]
#[kani::unwind(3)]
fn refute_dis_flush_then_clear_demotes() {
    let clock = Clock::new();
    let k: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(k, 4);
    let pool = mt.pool_ptr();
    let mut e = Entry::mem(k, pool, 4, None);
    e.read_ref = 1; // the write job's read ref (downgrade_reference at populate)
    let mut c = Ctx::new(Dm::new(&clock, [Some(e), None]), mt, Gpu::new(-1));
    let ext = Extents::default();
    ext.full.set(true);
    c.data_drives.borrow_mut().push(Drive { extents: ext, rw_stats: interfaces::ReadWriteStats::default() });
    c.bg_writer_in_flight = Some(std::cell::Cell::new(1));

    process_write_job(&c, k, 4); // the background writer thread runs the job
    let flushed = flush_to_ssd(&c);
    let cleared = clear_memory_tier(&c);

    let dropped = c.dm.get(k).is_none();
    let one = matches!(cleared, Ok(1));
    kani::cover!(flushed.is_ok() && one && dropped);
    assert!(flushed.is_ok() && one);
    assert!(dropped, "the entry was dropped, not converted to block-device state");
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

/// DIS-INITIALIZE-REGISTER-HOST-MEMORY. SPDK mode: the GPU and the memory tier are
/// ready and `register_host_memory` fails (a CUDA error). `initialize` succeeds but
/// reports the failure through `log_info` (lib.rs:1991-1995): no warning is logged.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_initialize_register_host_memory() {
    let clock = Clock::new();
    let gpu = Gpu::new(0);
    gpu.s.borrow_mut().fail_register = true;
    let c = Ctx::new(Dm::new(&clock, [None, None]), Mt::new(&clock, 16), gpu);
    c.initialized.set(false);

    let r = initialize(&c, true, false);

    let attempted_and_failed = c.gpu.s.borrow().registers == 1;
    let warned = c.log.warns.get() > 0;
    kani::cover!(r.is_ok() && attempted_and_failed && !warned);
    assert!(r.is_ok() && attempted_and_failed, "registration failed, init succeeded");
    assert!(!warned, "the failure was not reported as a warning");
}

// ---------------------------------------------------------------------------
// invariants
// ---------------------------------------------------------------------------

/// DIS-INV-EVICTION-EVENTS. The eviction-event channel is full (its consumer has not
/// drained the last event). The background memory-tier evictor demotes one entry; its
/// `try_send` of the Demoted event fails and is discarded (background.rs:539-544):
/// the event is lost and `eviction_dropped` is not incremented — unlike
/// `emit_eviction` (lib.rs:572-579), which counts it.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_inv_eviction_events() {
    let clock = Clock::new();
    let k: u64 = kani::any();
    let mt = Mt::new(&clock, 16).with_slot(k, 16);
    let pool = mt.pool_ptr();
    let dm = Dm::new(&clock, [Some(Entry::mem(k, pool, 16, Some(0))), None]);
    let mut c = Ctx::new(dm, mt, Gpu::new(-1));
    let tx = EventChan::bounded(1);
    assert!(tx
        .try_send(EvictionEvent { key: k.wrapping_add(1), reason: EvictionReason::Demoted })
        .is_ok());
    c.eviction_tx = Some(tx);

    let demoted = mt_evictor_pass(&c, 1, 0.70);

    let lost = c.eviction_tx.as_ref().unwrap().len() == 1; // only the old event
    let counted = c.eviction_dropped.get();
    kani::cover!(demoted == 1 && lost && counted == 0);
    assert!(demoted == 1, "one entry was demoted");
    assert!(lost, "its Demoted event did not fit the channel");
    assert!(counted == 0, "and was not counted as dropped");
}

/// DIS-INV-PIPELINE-DRAIN-ALL. A cold read of two objects (one segment each) at queue
/// depth 1. The first read fails; `pipelined_multi_object_zero_copy`
/// (pipeline.rs:591-624) records the error but keeps submitting: the second object's
/// read is sent AFTER the first error.
#[kani::proof]
#[kani::unwind(6)]
fn refute_dis_inv_pipeline_drain_all() {
    assume_fr019(4096, 512, 4096);
    let gpu = Gpu::new(0);
    let jobs = two_jobs();
    let chan = Chan::new([read_failed(0), read_ok(1), Cpl::Closed, Cpl::Closed]);

    let results = pipelined_multi_object_zero_copy(512, &gpu, &pipe_streams(), &chan, &jobs, 4096, 1);

    let sent_after_error = chan.sent.borrow().len() == 2;
    kani::cover!(results[0].is_err() && sent_after_error);
    assert!(results[0].is_err(), "the first read failed and is reported");
    assert!(sent_after_error, "a new read was submitted after the first error");
}

/// DIS-READ-WRITE-STATS-MONOTONIC. One data drive whose block device has counted
/// reads (telemetry build). `read_write_stats` sums over the CURRENT `data_drives`
/// (lib.rs:3866-3876); `shutdown` empties that list (lib.rs:2161-2164), so the next
/// call returns less than the previous one.
#[kani::proof]
#[kani::unwind(30)]
fn refute_dis_read_write_stats_monotonic() {
    let clock = Clock::new();
    let reads: u64 = kani::any();
    kani::assume(reads >= 1 && reads <= 1 << 40);
    let c = Ctx::new(Dm::new(&clock, [None, None]), Mt::new(&clock, 16), Gpu::new(-1));
    let mut st = interfaces::ReadWriteStats::default();
    st.read_ops = reads;
    c.data_drives.borrow_mut().push(Drive { extents: Extents::default(), rw_stats: st });

    let first = read_write_stats(&c);
    shutdown(&c).unwrap();
    let second = read_write_stats(&c);

    kani::cover!(second.read_ops < first.read_ops);
    assert!(second.read_ops < first.read_ops, "the read counter decreased");
}

/// DIS-TIER-STATS-BLOCKED-CAUSES. The memory tier is full with one slot that
/// `reserve_memory` reserved for a store that has not registered it in the dispatch
/// map yet (the reserve -> copy -> `copy_gpu_to_memory_completed` window). A second
/// `reserve_memory` scans it: `try_evict_to_block` returns `KeyNotFound`, and
/// `evict_one_clean` counts it in `evictions_blocked_unpersisted` (lib.rs:1076-1083) —
/// although no candidate was refused for lacking an SSD offset (REAL counters).
#[kani::proof]
#[kani::unwind(3)]
fn refute_dis_tier_stats_blocked_causes() {
    let clock = Clock::new();
    let k_reserved: u64 = kani::any();
    let k_new: u64 = kani::any();
    kani::assume(k_new != k_reserved);
    let mt = Mt::new(&clock, 8).with_slot(k_reserved, 8);
    let mut c = Ctx::new(Dm::new(&clock, [None, None]), mt, Gpu::new(-1));
    c.store_backpressure_ms = 0;
    let before = c.tier_counters.snapshot();

    let r = reserve_memory(&c, k_new, 8);

    let after = c.tier_counters.snapshot();
    let lacking_offset_candidates = 0u64; // the only candidate has no map entry at all
    let unpersisted = after.evictions_blocked_unpersisted - before.evictions_blocked_unpersisted;
    kani::cover!(r.is_err() && unpersisted == 1);
    assert!(r.is_err());
    assert!(
        unpersisted != lacking_offset_candidates,
        "blocked_unpersisted counted a candidate refused for another cause"
    );
}



