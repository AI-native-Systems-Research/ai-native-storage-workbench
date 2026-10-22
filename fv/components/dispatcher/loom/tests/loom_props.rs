//! Loom checks for the dispatcher concurrency properties
//! (statements: ../verif/unified_properties.yaml).
//!
//! Run one: `RUSTFLAGS="--cfg loom" cargo test --release --test loom_props <name> -- --exact`
//!
//! Each `verify_<id>` runs a scenario on the faithful mirror (`Mutation::None`);
//! its `verify_<id>__mutant` twin runs the SAME scenario and assertion on a
//! mirror with one requirement-relevant change and is expected to FAIL. A
//! failing `verify_` test is a finding: the mirrored code breaks the property.
#![allow(non_snake_case)]

use dispatcher_verif_loom::background::{
    mt_evictor_pass, ssd_evictor_pass, EvictionReason, EvictorConfig, WriteJob,
};
use dispatcher_verif_loom::dispatcher::{Dispatcher, DErr, Hook, ServedBy};
use dispatcher_verif_loom::dmap::{DispatchMap, Entry, Location, LookupResult};
use dispatcher_verif_loom::stand::{Ev, EvLog, ExtentManager, MemoryTier, SLOT};
use dispatcher_verif_loom::{CacheKey, Mutation};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

/// Preemption bound per model (None = exhaustive).
const BOUND_DEFAULT: Option<usize> = None;
/// Four threads (two populate+lookup clients, the writer, main); the
/// exhaustive search does not finish in 900 s, so it is bounded.
const BOUND_NO_CORRUPTION: Option<usize> = Some(3);

fn model<F: Fn() + Sync + Send + 'static>(bound: Option<usize>, f: F) {
    let mut b = loom::model::Builder::new();
    b.preemption_bound = bound;
    let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r = runs.clone();
    b.check(move || {
        r.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        f()
    });
    eprintln!(
        "loom: explored {} executions (preemption_bound = {:?})",
        runs.load(std::sync::atomic::Ordering::Relaxed),
        bound
    );
}

/// Anti-vacuity: counts executions in which the interesting branch was taken.
fn reached() -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0))
}

/// A resident, already written-through entry (refs 0, ssd_offset set).
fn seed_resident(dm: &DispatchMap, mt: &MemoryTier, key: CacheKey, data: u64, off: u64) -> usize {
    let slot = mt.insert(key, SLOT).unwrap();
    mt.write_slot(slot, data);
    dm.seed(
        key,
        Entry {
            location: Location::MemoryTier { pointer: slot, size: SLOT, ssd_offset: Some(off) },
            read_ref: 0,
            write_ref: 0,
        },
    );
    slot
}

fn plain(nslots: usize, drives: Vec<Arc<ExtentManager>>, m: Mutation) -> Arc<Dispatcher> {
    Arc::new(Dispatcher::new(
        Arc::new(DispatchMap::new(m)),
        Arc::new(MemoryTier::new(nslots)),
        drives,
        m,
    ))
}

// ---------------------------------------------------------------------------
// DIS-LOOKUP-CONCURRENT-READERS
// ---------------------------------------------------------------------------

fn concurrent_readers(m: Mutation) {
    // Two lookups of one cached key are inside their read sections (read
    // reference held, slot being served) at the same time: each waits there
    // until both have arrived. If readers excluded each other this deadlocks.
    model(BOUND_DEFAULT, move || {
        let gate = std::sync::Arc::new((Mutex::new(0usize), Condvar::new()));
        let g = gate.clone();
        let mut d = Dispatcher::new(
            Arc::new(DispatchMap::new(m)),
            Arc::new(MemoryTier::new(1)),
            vec![],
            m,
        );
        d.hook = Some(Box::new(move |h| {
            let Hook::ReadSection(_) = h;
            let (lock, cvar) = &*g;
            let mut n = lock.lock().unwrap();
            *n += 1;
            cvar.notify_all();
            while *n < 2 {
                n = cvar.wait(n).unwrap();
            }
        }));
        seed_resident(&d.dm, &d.mt, 1, 0x11, 0);
        let d = Arc::new(d);
        let d2 = d.clone();
        let t = thread::spawn(move || assert_eq!(d2.lookup(1, SLOT), Ok(0x11)));
        assert_eq!(d.lookup(1, SLOT), Ok(0x11));
        t.join().unwrap();
        let e = d.dm.entry(1).unwrap();
        assert_eq!((e.read_ref, e.write_ref), (0, 0), "both readers released");
    });
}

#[test]
fn verify_dis_lookup_concurrent_readers() {
    concurrent_readers(Mutation::None);
}

#[test]
fn verify_dis_lookup_concurrent_readers__mutant() {
    concurrent_readers(Mutation::LookupExclusive);
}

// ---------------------------------------------------------------------------
// DIS-LOOKUP-NO-DATA-CORRUPTION-CONCURRENT
// ---------------------------------------------------------------------------

fn no_data_corruption(m: Mutation) {
    // Two clients each populate their own key and look it up, concurrently with
    // the background writer persisting both. Each lookup must deliver its own
    // key's data, the SSD copy must be that data, and every slot access must be
    // race-free (loom's UnsafeCell checker), and everything completes.
    model(BOUND_NO_CORRUPTION, move || {
        let ext = Arc::new(ExtentManager::new(2));
        let d = plain(2, vec![ext.clone()], m);
        d.start_bg_writer();
        let mut ts = Vec::new();
        for (key, data) in [(1u64, 0xA1u64), (2, 0xB2)] {
            let d = d.clone();
            ts.push(thread::spawn(move || {
                assert_eq!(d.populate(key, data, SLOT), Ok(()));
                assert_eq!(d.lookup(key, SLOT), Ok(data), "lookup of key {key} got other data");
            }));
        }
        for t in ts {
            t.join().unwrap();
        }
        d.shutdown();
        assert_eq!(ext.data_of(1), Some(0xA1), "SSD copy of key 1");
        assert_eq!(ext.data_of(2), Some(0xB2), "SSD copy of key 2");
    });
}

#[test]
fn verify_dis_lookup_no_data_corruption_concurrent() {
    no_data_corruption(Mutation::None);
}

#[test]
fn verify_dis_lookup_no_data_corruption_concurrent__mutant() {
    no_data_corruption(Mutation::PublishBeforeCopy);
}

// ---------------------------------------------------------------------------
// DIS-RESERVE-MEMORY-BACKPRESSURE-RETRIES
// ---------------------------------------------------------------------------

fn backpressure(m: Mutation) {
    // A one-slot tier is full with key 1, which is not yet written through
    // (pending job holds its read reference), so it cannot be demoted. The
    // write-through runs concurrently and makes it demotable. With the deadline
    // still open, reserve_memory(2) must keep retrying and succeed.
    model(BOUND_DEFAULT, move || {
        let d = plain(1, vec![], m);
        let slot = d.mt.insert(1, SLOT).unwrap();
        d.mt.write_slot(slot, 0x1);
        d.dm.seed(
            1,
            Entry {
                location: Location::MemoryTier { pointer: slot, size: SLOT, ssd_offset: None },
                read_ref: 1,
                write_ref: 0,
            },
        );
        let w = d.clone();
        let t = thread::spawn(move || {
            Dispatcher::process_write_job(&w.dm, &w.mt, &w.drives, WriteJob {
                key: 1,
                size: SLOT,
                device_index: 0,
            });
        });
        let r = d.reserve_memory(2, SLOT, &|| false);
        t.join().unwrap();
        assert!(r.is_ok(), "reserve_memory gave up while the deadline was open: {r:?}");
        assert!(d.mt.contains(2));
        assert!(matches!(d.dm.entry(1).unwrap().location, Location::BlockDevice { .. }));
    });
}

#[test]
fn verify_dis_reserve_memory_backpressure_retries() {
    backpressure(Mutation::None);
}

#[test]
fn verify_dis_reserve_memory_backpressure_retries__mutant() {
    backpressure(Mutation::ReserveFailFast);
}

// ---------------------------------------------------------------------------
// DIS-BATCH-LOOKUP-SERVEDBY-CONCURRENT-DRAM / -OVERWRITE
// ---------------------------------------------------------------------------

/// Two batch_lookups of the same SSD-resident key (e.g. two TP ranks). One
/// wins the promotion; the other may lose the mt.insert race, wait for the
/// winner's promotion, and serve the key from DRAM.
fn concurrent_promotion(m: Mutation, check: fn(&dispatcher_verif_loom::dispatcher::Outcome)) {
    let hits = reached();
    let h = hits.clone();
    model(BOUND_DEFAULT, move || {
        let d = plain(2, vec![], m);
        d.dm.recover_extent(7, 0).unwrap();
        let d2 = d.clone();
        let t = thread::spawn(move || d2.batch_lookup(&[(7, SLOT)]));
        let mine = d.batch_lookup(&[(7, SLOT)]);
        let theirs = t.join().unwrap();
        for o in mine.iter().chain(theirs.iter()) {
            assert_eq!(o.result, Ok(()), "both lookups are hits");
            if o.overwritten {
                h.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            check(o);
        }
    });
    assert!(hits.load(std::sync::atomic::Ordering::Relaxed) > 0, "recovery path never reached");
}

fn servedby_concurrent_dram(m: Mutation) {
    concurrent_promotion(m, |o| {
        if o.overwritten && o.result.is_ok() {
            assert_eq!(
                o.served_by,
                ServedBy::Dram,
                "served from DRAM after waiting for the concurrent promotion, attributed {:?}",
                o.served_by
            );
        }
    });
}

#[test]
fn verify_dis_batch_lookup_servedby_concurrent_dram() {
    servedby_concurrent_dram(Mutation::None);
}

#[test]
fn verify_dis_batch_lookup_servedby_concurrent_dram__mutant() {
    servedby_concurrent_dram(Mutation::RecoveryWrongTier);
}

fn servedby_overwrite(m: Mutation) {
    concurrent_promotion(m, |o| {
        if o.overwritten {
            assert!(o.tier_rewritten, "result rewritten without rewriting its attribution");
            // The rewritten pair must agree: an Ok produced by the DRAM re-serve
            // is attributed Dram.
            if o.result.is_ok() {
                assert_eq!(o.served_by, ServedBy::Dram, "rewritten result and attribution disagree");
            }
        }
    });
}

#[test]
fn verify_dis_batch_lookup_servedby_overwrite() {
    servedby_overwrite(Mutation::None);
}

#[test]
fn verify_dis_batch_lookup_servedby_overwrite__mutant() {
    servedby_overwrite(Mutation::RecoveryWrongTier);
}

// ---------------------------------------------------------------------------
// DIS-REMOVE-NO-WAIT
// ---------------------------------------------------------------------------

fn remove_no_wait(m: Mutation) {
    // (A) The key was just populated; its write-through job is queued and the
    // writer runs concurrently with remove. Remove must go ahead and succeed.
    model(BOUND_DEFAULT, move || {
        let d = plain(1, vec![], m);
        d.start_bg_writer();
        d.populate(1, 0x1, SLOT).unwrap();
        let d2 = d.clone();
        let t = thread::spawn(move || d2.remove(1));
        let r = t.join().unwrap();
        d.shutdown();
        assert_eq!(r, Ok(()), "remove did not proceed past the pending write-through");
        assert!(d.dm.entry(1).is_none() && !d.mt.contains(1));
    });
    // (B) The write-through is still pending and makes no progress while remove
    // runs: remove must return (not wait for it) and succeed.
    model(BOUND_DEFAULT, move || {
        let d = plain(1, vec![], m);
        d.populate(1, 0x1, SLOT).unwrap(); // job pending: read_ref held
        let r = d.remove(1);
        assert_eq!(r, Ok(()), "remove did not proceed past the pending write-through");
        // The write-through then runs late against a removed key.
        Dispatcher::process_write_job(&d.dm, &d.mt, &d.drives, WriteJob {
            key: 1,
            size: SLOT,
            device_index: 0,
        });
    });
}

#[test]
fn verify_dis_remove_no_wait() {
    remove_no_wait(Mutation::None);
}

#[test]
fn verify_dis_remove_no_wait__mutant() {
    remove_no_wait(Mutation::RemoveWaitsForRefs);
}

// ---------------------------------------------------------------------------
// DIS-REMOVE-ORDER
// ---------------------------------------------------------------------------

fn remove_order(m: Mutation) {
    // A resident key with an SSD copy (written through): remove must free the
    // memory-tier slot, then delete the dispatch-map entry, then free the extent.
    // A concurrent reader may make remove refuse; the order is checked when it
    // succeeds.
    model(BOUND_DEFAULT, move || {
        let log = std::sync::Arc::new(EvLog::default());
        let mut ext = ExtentManager::new(1);
        ext.log = Some(log.clone());
        let ext = Arc::new(ext);
        let mut dm = DispatchMap::new(m);
        dm.log = Some(log.clone());
        let mut mt = MemoryTier::new(1);
        mt.log = Some(log.clone());
        let off = ext.seed(3, 0x3);
        seed_resident(&dm, &mt, 3, 0x3, off);
        let d = Arc::new(Dispatcher::new(Arc::new(dm), Arc::new(mt), vec![ext.clone()], m));
        log.take();
        let d2 = d.clone();
        let t = thread::spawn(move || {
            let _ = d2.lookup(3, SLOT);
        });
        let r = d.remove(3);
        t.join().unwrap();
        if r.is_ok() {
            assert_eq!(
                log.take(),
                vec![Ev::MtFree(3), Ev::DmRemove(3), Ev::ExtentFree(off)],
                "remove steps out of order (expected slot, map entry, extent)"
            );
        }
    });
}

#[test]
fn verify_dis_remove_order() {
    remove_order(Mutation::None);
}

#[test]
fn verify_dis_remove_order__mutant() {
    remove_order(Mutation::RemoveSkipsSlotFree);
}

// ---------------------------------------------------------------------------
// DIS-FLUSH-THEN-CLEAR-DEMOTES
// ---------------------------------------------------------------------------

fn flush_then_clear(m: Mutation) {
    // Two keys populated (write-through queued to the background writer), then
    // flush_to_ssd, then clear_memory_tier: both entries must be in
    // block-device state (data preserved on SSD), not dropped.
    model(BOUND_DEFAULT, move || {
        let ext = Arc::new(ExtentManager::new(2));
        let d = plain(2, vec![ext.clone()], m);
        d.start_bg_writer();
        d.populate(1, 0x1, SLOT).unwrap();
        d.populate(2, 0x2, SLOT).unwrap();
        d.flush_to_ssd().unwrap();
        let cleared = d.clear_memory_tier();
        assert_eq!(cleared, 2);
        for (k, v) in [(1u64, 0x1u64), (2, 0x2)] {
            let e = d.dm.entry(k);
            assert!(
                matches!(e, Some(Entry { location: Location::BlockDevice { .. }, .. })),
                "key {k} not in block-device state after flush + clear: {e:?}"
            );
            assert_eq!(ext.data_of(k), Some(v));
        }
        d.shutdown();
    });
}

#[test]
fn verify_dis_flush_then_clear_demotes() {
    flush_then_clear(Mutation::None);
}

#[test]
fn verify_dis_flush_then_clear_demotes__mutant() {
    flush_then_clear(Mutation::SettleSkipsConvert);
}

// ---------------------------------------------------------------------------
// DIS-INV-EVICTION-EVENTS
// ---------------------------------------------------------------------------

fn eviction_events(m: Mutation) {
    // The memory-tier evictor demotes two entries into a capacity-1 event
    // channel while the serve layer drains it concurrently (TakeEvents, and the
    // dropped counter). Every demotion must surface as a Demoted event or be
    // counted as dropped; the evictor never blocks.
    model(BOUND_DEFAULT, move || {
        let d = plain(2, vec![], m);
        seed_resident(&d.dm, &d.mt, 1, 0x1, 0);
        seed_resident(&d.dm, &d.mt, 2, 0x2, SLOT as u64);
        let rx = d.create_eviction_channel(1);
        let (d2, rx2) = (d.clone(), rx.clone());
        let consumer = thread::spawn(move || {
            let got: Vec<_> = rx2.try_recv().into_iter().collect();
            (got, d2.eviction_dropped_count())
        });
        let cfg = EvictorConfig { threshold: 0.5, low_watermark: 0.0, batch_size: 2 };
        let rep = mt_evictor_pass(&d.dm, &d.mt, &cfg, &d.eviction_tx, 0, m);
        let (mut got, mut dropped) = consumer.join().unwrap();
        while let Some(e) = rx.try_recv() {
            got.push(e);
        }
        dropped += d.eviction_dropped_count();
        assert_eq!(rep.evicted.len(), 2);
        for e in &got {
            assert_eq!(e.reason, EvictionReason::Demoted, "demotion event with wrong reason");
            assert!(rep.evicted.contains(&e.key));
        }
        assert_eq!(
            got.len() as u64 + dropped,
            rep.evicted.len() as u64,
            "demoted {} entries, {} events delivered, {} counted dropped",
            rep.evicted.len(),
            got.len(),
            dropped
        );
    });
}

#[test]
fn verify_dis_inv_eviction_events() {
    eviction_events(Mutation::None);
}

#[test]
fn verify_dis_inv_eviction_events__mutant() {
    eviction_events(Mutation::EvictorWrongReason);
}

// ---------------------------------------------------------------------------
// DIS-INV-MT-EVICTOR-WATERMARK
// ---------------------------------------------------------------------------

fn mt_watermark(m: Mutation) {
    // A full 4-slot tier (utilization 1.0 > threshold 0.7) of written-through
    // entries; a concurrent reader (lookup's dispatch-map read reference) pins
    // key 1 for part of the pass. The pass
    // must demote oldest-first until utilization < low_watermark (0.2) or the
    // scan runs out of candidates (only the pinned key may be skipped).
    model(BOUND_DEFAULT, move || {
        let d = plain(4, vec![], m);
        for k in 1..=4u64 {
            seed_resident(&d.dm, &d.mt, k, k, k * SLOT as u64);
        }
        let d2 = d.clone();
        let reader = thread::spawn(move || {
            if d2.dm.lookup(1).is_ok() {
                let _ = d2.dm.release_read(1);
            }
        });
        let cfg = EvictorConfig { threshold: 0.7, low_watermark: 0.2, batch_size: 1 };
        let rep = mt_evictor_pass(&d.dm, &d.mt, &cfg, &d.eviction_tx, 0, m);
        reader.join().unwrap();
        assert!(rep.scanned, "utilization above threshold but no scan");
        let mut it = rep.candidates.iter();
        assert!(rep.evicted.iter().all(|k| it.any(|c| c == k)), "not oldest-first");
        if rep.stopped_at_watermark {
            assert!(rep.last_util < cfg.low_watermark, "stopped at {} >= low_watermark", rep.last_util);
        } else {
            for c in &rep.candidates {
                assert!(*c == 1 || rep.evicted.contains(c), "evictable key {c} skipped");
            }
        }
        for k in &rep.evicted {
            assert!(!d.mt.contains(*k));
            assert!(matches!(d.dm.entry(*k).unwrap().location, Location::BlockDevice { .. }));
        }
    });
}

#[test]
fn verify_dis_inv_mt_evictor_watermark() {
    mt_watermark(Mutation::None);
}

#[test]
fn verify_dis_inv_mt_evictor_watermark__mutant() {
    mt_watermark(Mutation::MtEvictorStopAfterOne);
}

// ---------------------------------------------------------------------------
// DIS-INV-SSD-EVICTOR-WATERMARK
// ---------------------------------------------------------------------------

fn ssd_watermark(m: Mutation) {
    // A full 4-extent drive (utilization 1.0 > threshold 0.7) of block-device
    // entries; a concurrent reader (check-style lookup) holds key 1 for part of
    // the pass. The pass must evict oldest-first until utilization <
    // low_watermark (0.2) or the batch is exhausted (only key 1 may be skipped).
    model(BOUND_DEFAULT, move || {
        let ext = Arc::new(ExtentManager::new(4));
        let d = plain(1, vec![ext.clone()], m);
        for k in 1..=4u64 {
            let off = ext.seed(k, k);
            d.dm.recover_extent(k, off).unwrap();
        }
        let d2 = d.clone();
        let reader = thread::spawn(move || {
            if d2.dm.lookup(1).is_ok() {
                let _ = d2.dm.release_read(1);
            }
        });
        let cfg = EvictorConfig { threshold: 0.7, low_watermark: 0.2, batch_size: 4 };
        let rep = ssd_evictor_pass(&d.dm, &d.mt, &d.drives, &cfg, None, m);
        reader.join().unwrap();
        assert!(rep.scanned, "utilization above threshold but no scan");
        let mut it = rep.candidates.iter();
        assert!(rep.evicted.iter().all(|k| it.any(|c| c == k)), "not oldest-first");
        if rep.stopped_at_watermark {
            assert!(rep.last_util < cfg.low_watermark, "stopped at {} >= low_watermark", rep.last_util);
        } else {
            assert_eq!(rep.candidates.len(), 4);
            for c in &rep.candidates {
                assert!(*c == 1 || rep.evicted.contains(c), "evictable key {c} skipped");
            }
        }
        for k in &rep.evicted {
            assert!(d.dm.entry(*k).is_none());
            assert_eq!(ext.data_of(*k), None);
        }
    });
}

#[test]
fn verify_dis_inv_ssd_evictor_watermark() {
    ssd_watermark(Mutation::None);
}

#[test]
fn verify_dis_inv_ssd_evictor_watermark__mutant() {
    ssd_watermark(Mutation::SsdEvictorStopAfterOne);
}

// ---------------------------------------------------------------------------
// DIS-SPIN-CLEAR-MT-KEEPS-PINNED
// ---------------------------------------------------------------------------

/// Checks shared by both scenarios, at the moment clear_memory_tier returns.
/// `keys` are the entries present before the clear; `count` is what it returned.
fn check_cleared(d: &Dispatcher, keys: &[CacheKey], count: usize) {
    let mut really_cleared = 0;
    for &k in keys {
        match d.dm.entry(k) {
            None => really_cleared += 1,
            Some(Entry { location: Location::BlockDevice { .. }, .. }) => really_cleared += 1,
            Some(Entry { location: Location::MemoryTier { pointer, .. }, .. }) => {
                assert_eq!(
                    d.mt.peek(k).map(|(p, _)| p),
                    Some(pointer),
                    "key {k}: dispatch-map entry points at a released memory-tier slot"
                );
                panic!("key {k} still MemoryTier when clear_memory_tier returned");
            }
        }
    }
    assert_eq!(count, really_cleared, "returned count != entries really cleared");
}

fn clear_keeps_pinned(m: Mutation) {
    // (A) Lookup pin: key 1 is resident and written through. A lookup of it
    // runs concurrently with clear_memory_tier; while the lookup holds its read
    // pin, clear cannot demote it.
    let pinned = reached();
    let p = pinned.clone();
    model(BOUND_DEFAULT, move || {
        let d = plain(1, vec![], m);
        seed_resident(&d.dm, &d.mt, 1, 0x11, 0);
        let d2 = d.clone();
        // lookup_async's warm branch (src/lib.rs:3094): pin, H2D read of the
        // slot, release. A cold (BlockDevice) result after the demotion is
        // kept simple here: only the pin is released (promote_and_serve needs
        // a drive and a caller size; see the lookup cold-failure tests).
        let t = thread::spawn(move || match d2.dm.lookup(1) {
            Ok(LookupResult::MemoryTier { pointer, .. }) => {
                let _ = d2.mt.read_slot(pointer);
                let _ = d2.dm.release_read(1);
                d2.mt.touch(1);
            }
            Ok(LookupResult::BlockDevice { .. }) => {
                let _ = d2.dm.release_read(1);
            }
            _ => {}
        });
        let count = d.clear_memory_tier();
        if d.dm.entry(1).is_some_and(|e| e.read_ref > 0) {
            p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        check_cleared(&d, &[1], count);
        t.join().unwrap();
        assert_eq!(d.mt.stale_reads(), 0, "a lookup read a released memory-tier slot");
    });
    assert!(pinned.load(std::sync::atomic::Ordering::Relaxed) > 0, "lookup pin never overlapped clear");
    // (B) Writer pin: key 2 was just populated; its write-through job holds the
    // read pin and the background writer runs concurrently with the clear.
    model(BOUND_DEFAULT, move || {
        let ext = Arc::new(ExtentManager::new(1));
        let d = plain(1, vec![ext.clone()], m);
        d.start_bg_writer();
        d.populate(2, 0x22, SLOT).unwrap();
        let count = d.clear_memory_tier();
        check_cleared(&d, &[2], count);
        d.shutdown();
        if let Some(Entry { location: Location::MemoryTier { pointer, .. }, .. }) = d.dm.entry(2) {
            assert_eq!(d.mt.peek(2).map(|(p, _)| p), Some(pointer), "entry points at a released slot");
        }
        assert_eq!(d.mt.stale_reads(), 0, "the writer read a released memory-tier slot");
    });
}

#[test]
fn verify_dis_spin_clear_mt_keeps_pinned() {
    clear_keeps_pinned(Mutation::None);
}

#[test]
fn verify_dis_spin_clear_mt_keeps_pinned__mutant() {
    clear_keeps_pinned(Mutation::ClearFreesFirst);
}

// ---------------------------------------------------------------------------
// DIS-FLUSH-REPORTS-FAILED-WRITE
// ---------------------------------------------------------------------------

/// The property at the moment flush_to_ssd returned `r`, for `keys` populated
/// before the call (with `data`). On `Ok(n)`: every key has an SSD offset and
/// its data on the drive, `n` counts the keys with an SSD offset, and a
/// clear_memory_tier then demotes every key instead of dropping its only copy.
fn check_flush(d: &Dispatcher, ext: &ExtentManager, keys: &[(CacheKey, u64)], r: Result<usize, dispatcher_verif_loom::dispatcher::DErr>) {
    let Ok(n) = r else { return };
    let mut with_offset = 0;
    for &(k, v) in keys {
        let e = d.dm.entry(k);
        assert!(
            matches!(e, Some(Entry { location: Location::MemoryTier { ssd_offset: Some(_), .. }, .. })),
            "flush_to_ssd returned Ok but key {k} has no SSD offset: {e:?}"
        );
        assert_eq!(ext.data_of(k), Some(v), "flush_to_ssd returned Ok but key {k} is not on the drive");
        with_offset += 1;
    }
    assert_eq!(n, with_offset, "flush_to_ssd returned Ok({n}), {with_offset} entries have an SSD offset");
    let _ = d.clear_memory_tier();
    for &(k, v) in keys {
        assert!(
            matches!(d.dm.entry(k), Some(Entry { location: Location::BlockDevice { .. }, .. })),
            "clear_memory_tier after an Ok flush dropped key {k}"
        );
        assert_eq!(ext.data_of(k), Some(v));
    }
}

fn flush_reports_failed_write(m: Mutation) {
    // (A) Keys 1 and 2 are populated; the drive fails the write of key 2. The
    // background writer runs concurrently with flush_to_ssd. Since a
    // write-through failed, flush must not return Ok (and if it did, key 2
    // would have its only copy in DRAM).
    model(BOUND_DEFAULT, move || {
        let mut ext = ExtentManager::new(2);
        ext.fail_write_key = Some(2);
        let ext = Arc::new(ext);
        let d = plain(2, vec![ext.clone()], m);
        d.start_bg_writer();
        d.populate(1, 0x1, SLOT).unwrap();
        d.populate(2, 0x2, SLOT).unwrap();
        let r = d.flush_to_ssd();
        check_flush(&d, &ext, &[(1, 0x1), (2, 0x2)], r);
        d.shutdown();
    });
    // (B) No write fails: flush must return Ok, and the count is the number of
    // entries with an SSD offset, whatever the writer had done before the call.
    model(BOUND_DEFAULT, move || {
        let ext = Arc::new(ExtentManager::new(2));
        let d = plain(2, vec![ext.clone()], m);
        d.start_bg_writer();
        d.populate(1, 0x1, SLOT).unwrap();
        d.populate(2, 0x2, SLOT).unwrap();
        let r = d.flush_to_ssd();
        assert!(r.is_ok(), "flush_to_ssd failed with no write error: {r:?}");
        check_flush(&d, &ext, &[(1, 0x1), (2, 0x2)], r);
        d.shutdown();
    });
}

#[test]
fn verify_dis_flush_reports_failed_write() {
    flush_reports_failed_write(Mutation::None);
}

#[test]
fn verify_dis_flush_reports_failed_write__mutant() {
    flush_reports_failed_write(Mutation::FlushNoWait);
}

// ---------------------------------------------------------------------------
// DIS-LOOKUP-COLD-FAILURE-RELEASES-SLOT
// ---------------------------------------------------------------------------

/// The property at the moment the operation under test returned: the memory
/// tier holds no slot for `key` that the dispatch map does not point to (and
/// no entry points at a slot the tier no longer holds).
fn check_no_orphan_slot(d: &Dispatcher, key: CacheKey) {
    if d.mt.contains(key) {
        match d.dm.entry(key) {
            Some(Entry { location: Location::MemoryTier { pointer, .. }, .. }) => {
                assert_eq!(
                    d.mt.peek(key).map(|(p, _)| p),
                    Some(pointer),
                    "key {key}: dispatch-map entry points at a released memory-tier slot"
                );
            }
            other => panic!(
                "memory tier holds a slot for key {key} that the dispatch map does not point to: {other:?}"
            ),
        }
    }
}

/// The key stays SSD resident: a block-device entry at `off`.
fn check_ssd_resident(d: &Dispatcher, key: CacheKey, off: u64) {
    match d.dm.entry(key) {
        Some(Entry { location: Location::BlockDevice { offset }, .. }) => assert_eq!(
            offset, off,
            "key {key} did not stay SSD resident at its offset"
        ),
        other => panic!("key {key} is not SSD resident (BlockDevice) any more: {other:?}"),
    }
}

/// Seeds the shared scenario state on a fresh dispatcher: key 7 SSD-resident
/// on drive 0 (data 0x77), key 8 warm in the memory tier. Returns key 7's
/// SSD offset.
fn seed_cold_warm(d: &Dispatcher) -> u64 {
    let off = d.drives[0].seed(7, 0x77);
    d.dm.recover_extent(7, off).unwrap();
    seed_resident(&d.dm, &d.mt, 8, 0x88, 0);
    off
}

/// The lookup scenario: a cold lookup of key 7 (holding its dispatch-map read
/// pin across the whole promotion, src/lib.rs:3127-3154) runs with a warm
/// lookup of key 8 in flight. `mk` builds the dispatcher (with the scenario's
/// failure injection); `r` is the expected outcome; `post` runs the
/// scenario-specific assertion once the lookup returned.
fn lookup_cold_scenario(
    mk: impl Fn() -> Arc<Dispatcher> + Send + Sync + 'static,
    r: Result<u64, DErr>,
    post: impl Fn(&Dispatcher, u64) + Send + Sync + 'static,
) {
    let hits = reached();
    let h = hits.clone();
    model(BOUND_DEFAULT, move || {
        let d = mk();
        let off = seed_cold_warm(&d);
        let d2 = d.clone();
        let warm = thread::spawn(move || {
            assert_eq!(d2.lookup(8, SLOT), Ok(0x88), "concurrent warm lookup");
        });
        let cold = d.lookup(7, SLOT);
        warm.join().unwrap();
        assert_eq!(cold, r, "cold lookup of key 7");
        check_no_orphan_slot(&d, 7);
        check_no_orphan_slot(&d, 8);
        post(&d, off);
        h.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    assert!(hits.load(std::sync::atomic::Ordering::Relaxed) > 0, "scenario never ran");
}

fn lookup_cold_failure_releases_slot(m: Mutation) {
    // (A) Control: nothing fails — the cold key is promoted and served.
    lookup_cold_scenario(
        move || plain(2, vec![Arc::new(ExtentManager::new(2))], m),
        Ok(0x77),
        |d, _| {
            let e = d.dm.entry(7);
            assert!(
                matches!(e, Some(Entry { location: Location::MemoryTier { .. }, .. })),
                "control: key 7 not promoted into the memory tier: {e:?}"
            );
        },
    );
    // (B) The SSD read of the cold key fails after evict_and_insert reserved
    // the slot (src/lib.rs:888): the reserved slot must be released.
    lookup_cold_scenario(
        move || {
            let mut ext = ExtentManager::new(2);
            ext.fail_read_key = Some(7);
            plain(2, vec![Arc::new(ext)], m)
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
    // (C) The channel checkout fails after the slot was reserved
    // (src/lib.rs:836): the reserved slot must be released.
    lookup_cold_scenario(
        move || {
            let mut ext = ExtentManager::new(2);
            ext.channel_pool.fail_connect = true;
            plain(2, vec![Arc::new(ext)], m)
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
    // (D) The dispatch-map promotion fails after the slot was reserved and
    // read (src/lib.rs:909): the reserved slot must be released and the key
    // stays SSD resident.
    lookup_cold_scenario(
        move || {
            let mut dm = DispatchMap::new(m);
            dm.fail_promote_key = Some(7);
            Arc::new(Dispatcher::new(
                Arc::new(dm),
                Arc::new(MemoryTier::new(2)),
                vec![Arc::new(ExtentManager::new(2))],
                m,
            ))
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
}

/// `check_ssd_resident` as a `lookup_cold_scenario` post-check.
fn check_ssd_resident_at(d: &Dispatcher, off: u64) {
    check_ssd_resident(d, 7, off);
}

#[test]
fn verify_dis_lookup_cold_failure_releases_slot() {
    lookup_cold_failure_releases_slot(Mutation::None);
}

#[test]
fn verify_dis_lookup_cold_failure_releases_slot__mutant() {
    lookup_cold_failure_releases_slot(Mutation::LookupColdSkipsPromote);
}

// ---------------------------------------------------------------------------
// DIS-PROMOTE-READ-FAILURE-RELEASES
// ---------------------------------------------------------------------------

/// The promote_to_memory_tier scenario: key 7 is SSD-resident, key 8 is warm;
/// a concurrent warm lookup of key 8 runs while the prefetch thread reads
/// key 7 from the SSD and promotes it.
fn promote_read_scenario(
    mk: impl Fn() -> Arc<Dispatcher> + Send + Sync + 'static,
    post: impl Fn(&Dispatcher, u64) + Send + Sync + 'static,
) {
    let hits = reached();
    let h = hits.clone();
    model(BOUND_DEFAULT, move || {
        let d = mk();
        let off = seed_cold_warm(&d);
        let d2 = d.clone();
        let warm = thread::spawn(move || {
            assert_eq!(d2.lookup(8, SLOT), Ok(0x88), "concurrent warm lookup");
        });
        Dispatcher::promote_to_memory_tier(d.clone(), &[7, 8]);
        warm.join().unwrap();
        check_no_orphan_slot(&d, 7);
        check_no_orphan_slot(&d, 8);
        post(&d, off);
        h.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    assert!(hits.load(std::sync::atomic::Ordering::Relaxed) > 0, "scenario never ran");
}

fn promote_read_failure_releases(m: Mutation) {
    // (A) Control: nothing fails — the key is promoted into the memory tier.
    promote_read_scenario(
        move || plain(2, vec![Arc::new(ExtentManager::new(2))], m),
        |d, _| {
            let e = d.dm.entry(7);
            assert!(
                matches!(e, Some(Entry { location: Location::MemoryTier { .. }, .. })),
                "control: key 7 not promoted into the memory tier: {e:?}"
            );
        },
    );
    // (B) The SSD read fails (src/lib.rs:3905): the slot obtained for the key
    // is released and the key remains SSD resident.
    promote_read_scenario(
        move || {
            let mut ext = ExtentManager::new(2);
            ext.fail_read_key = Some(7);
            plain(2, vec![Arc::new(ext)], m)
        },
        check_ssd_resident_at,
    );
    // (C) The dispatch-map promotion fails (src/lib.rs:3917): the slot
    // obtained for the key is released and the key remains SSD resident.
    promote_read_scenario(
        move || {
            let mut dm = DispatchMap::new(m);
            dm.fail_promote_key = Some(7);
            Arc::new(Dispatcher::new(
                Arc::new(dm),
                Arc::new(MemoryTier::new(2)),
                vec![Arc::new(ExtentManager::new(2))],
                m,
            ))
        },
        check_ssd_resident_at,
    );
}

#[test]
fn verify_dis_promote_read_failure_releases() {
    promote_read_failure_releases(Mutation::None);
}

#[test]
fn verify_dis_promote_read_failure_releases__mutant() {
    promote_read_failure_releases(Mutation::PromoteSkipsReadFailRelease);
}

// ---------------------------------------------------------------------------
// DIS-BATCH-LOOKUP-COLD-FAILURE-RELEASES-SLOT
// ---------------------------------------------------------------------------

/// The batch_lookup scenario: a batch of the cold key 7 and the warm key 8
/// runs with a concurrent warm lookup of key 8 in flight; the cold key goes
/// through the pooled cold path. `expect` is key 7's batch outcome.
fn batch_cold_scenario(
    mk: impl Fn() -> Arc<Dispatcher> + Send + Sync + 'static,
    expect: Result<(), DErr>,
    post: impl Fn(&Dispatcher, u64) + Send + Sync + 'static,
) {
    let hits = reached();
    let h = hits.clone();
    model(BOUND_DEFAULT, move || {
        let d = mk();
        let off = seed_cold_warm(&d);
        let d2 = d.clone();
        let warm = thread::spawn(move || {
            assert_eq!(d2.lookup(8, SLOT), Ok(0x88), "concurrent warm lookup");
        });
        let out = d.batch_lookup(&[(7, SLOT), (8, SLOT)]);
        warm.join().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].result, expect, "batch outcome of the cold key 7");
        check_no_orphan_slot(&d, 7);
        check_no_orphan_slot(&d, 8);
        post(&d, off);
        h.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    assert!(hits.load(std::sync::atomic::Ordering::Relaxed) > 0, "scenario never ran");
}

fn batch_lookup_cold_failure_releases_slot(m: Mutation) {
    // (A) Control: nothing fails — the cold key is promoted and served.
    batch_cold_scenario(
        move || plain(2, vec![Arc::new(ExtentManager::new(2))], m),
        Ok(()),
        |d, _| {
            let e = d.dm.entry(7);
            assert!(
                matches!(e, Some(Entry { location: Location::MemoryTier { .. }, .. })),
                "control: key 7 not promoted into the memory tier: {e:?}"
            );
        },
    );
    // (B) The NVMe read of the cold key fails (src/lib.rs:2770,
    // `Err(e) => Err(e.clone())`): the slot must be released.
    batch_cold_scenario(
        move || {
            let mut ext = ExtentManager::new(2);
            ext.fail_read_key = Some(7);
            plain(2, vec![Arc::new(ext)], m)
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
    // (C) The channel checkout fails (src/lib.rs:2705-2710): the slot must be
    // released.
    batch_cold_scenario(
        move || {
            let mut ext = ExtentManager::new(2);
            ext.channel_pool.fail_connect = true;
            plain(2, vec![Arc::new(ext)], m)
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
    // (D) The stream setup fails (src/lib.rs:2677-2687): the slot must be
    // released.
    batch_cold_scenario(
        move || {
            let mut d = Dispatcher::new(
                Arc::new(DispatchMap::new(m)),
                Arc::new(MemoryTier::new(2)),
                vec![Arc::new(ExtentManager::new(2))],
                m,
            );
            d.gpu_fail_create_stream = true;
            Arc::new(d)
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
    // (E) The dispatch-map promotion fails (src/lib.rs:2747): the slot must be
    // released and the key stays SSD resident.
    batch_cold_scenario(
        move || {
            let mut dm = DispatchMap::new(m);
            dm.fail_promote_key = Some(7);
            Arc::new(Dispatcher::new(
                Arc::new(dm),
                Arc::new(MemoryTier::new(2)),
                vec![Arc::new(ExtentManager::new(2))],
                m,
            ))
        },
        Err(DErr::IoError),
        check_ssd_resident_at,
    );
}

#[test]
fn verify_dis_batch_lookup_cold_failure_releases_slot() {
    batch_lookup_cold_failure_releases_slot(Mutation::None);
}

#[test]
fn verify_dis_batch_lookup_cold_failure_releases_slot__mutant() {
    batch_lookup_cold_failure_releases_slot(Mutation::BatchColdSkipsPromote);
}
