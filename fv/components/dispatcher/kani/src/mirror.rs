//! DISCLOSED MIRROR of the dispatcher code paths the work-list properties are about.
//!
//! Why a mirror: the real `DispatcherComponent` is constructible from outside the
//! crate (`new_default()`, pub fields), and was tried first. Under Kani, even after
//! stubbing the interface-map initialiser, `RandomState`, `thread::Builder::spawn`
//! and `fmt`, a single real `batch_lookup` call on one key exceeded 400 s of CBMC:
//! every receptacle call goes through `dyn Trait`, and CBMC's function-pointer
//! removal pulls most of the 7 kLoC crate into the program (`-Z restrict-vtable`
//! spends ~90 s in goto-instrument alone). So each method is transcribed here
//! statement by statement, against the concrete collaborator models in
//! [`crate::model`]. Line references are to `components/dispatcher/src/` at the pin.
//!
//! What is NOT transcribed (and why it cannot change the properties):
//! * `format!` payloads of error variants and log lines -> `String::new()`; only the
//!   variant and the fact that a log call happened are observed.
//! * timing / metrics (`Instant`, `pipeline_metrics`, telemetry) — no effect on
//!   results.
//! * `#[cfg(feature = "integrity-check")]` blocks — feature off by default.
//! * `DmaBuffer::from_raw` wrappers around memory-tier pointers: modelled by their
//!   only failure (null pointer -> `Err`).
//! * The drive-backed cold branch of `batch_lookup` (lib.rs:2509-2739) is not in
//!   [`batch_lookup`]; the drive pipelines it calls are mirrored separately
//!   ([`pipelined_multi_object_zero_copy`], [`scatter_gather_multi_drive_zero_copy`]).
//!
//! The REAL `dispatcher::TierEventCounters`, `dispatcher::io_segmenter::segment_io`
//! and `interfaces::ReadWriteStats::merge_from` are called directly.

use std::cell::Cell;

use dispatcher::TierEventCounters;
use interfaces::{
    CacheKey, DispatchMapError, DispatcherError, EvictionEvent, EvictionReason, GpuStream,
    IpcHandle, LookupOutcome, LookupResult, MemoryTierError, ReadWriteStats, ServedBy,
};

use crate::model::*;

/// Nondeterministic choice (timing the code reads from the clock).
pub fn nondet_bool() -> bool {
    #[cfg(kani)]
    {
        kani::any()
    }
    #[cfg(not(kani))]
    {
        false
    }
}

/// One data drive (lib.rs:277-284): its extent manager and its block device's
/// read/write counters.
pub struct Drive {
    pub extents: Extents,
    pub rw_stats: ReadWriteStats,
}

#[derive(Clone, Copy)]
pub struct DeviceStreams {
    pub warm: u64,
    pub store: u64,
    pub pipe: [u64; 2],
}

/// Mirror of the `DispatcherComponent` state the mirrored methods read (lib.rs:404-433).
pub struct Ctx<'c> {
    pub dm: Dm<'c>,
    pub mt: Mt<'c>,
    pub gpu: Gpu,
    pub remote: Option<Remote>,
    pub log: Logger,
    pub initialized: Cell<bool>,
    pub data_drives: RefCell<Vec<Drive>>,
    pub warm_stream: Cell<u64>,
    pub max_eviction_attempts: usize,
    pub store_backpressure_ms: u64,
    pub eviction_tx: Option<EventChan>,
    pub eviction_dropped: Cell<u64>,
    /// `bg_writer`: in-flight count of the (single) background writer, if started.
    pub bg_writer_in_flight: Option<Cell<usize>>,
    pub tier_counters: TierEventCounters,
    /// The process-global `DEVICE_STREAMS` map (lib.rs:463-465), one device.
    pub device_streams: RefCell<Option<(i32, DeviceStreams)>>,
}

impl<'c> Ctx<'c> {
    /// The post-`initialize` state in memory-tier-only mode (lib.rs:1884-2110 with no
    /// ISPDKEnv and no factory): initialized, no drives, no warm stream, writer idle.
    pub fn new(dm: Dm<'c>, mt: Mt<'c>, gpu: Gpu) -> Self {
        Ctx {
            dm,
            mt,
            gpu,
            remote: None,
            log: Logger::default(),
            initialized: Cell::new(true),
            data_drives: RefCell::new(Vec::new()),
            warm_stream: Cell::new(0),
            max_eviction_attempts: 2048,
            store_backpressure_ms: 5000,
            eviction_tx: None,
            eviction_dropped: Cell::new(0),
            bg_writer_in_flight: Some(Cell::new(0)),
            tier_counters: TierEventCounters::default(),
            device_streams: RefCell::new(None),
        }
    }

    fn ensure_initialized(&self) -> Result<(), DispatcherError> {
        if !self.initialized.get() {
            return Err(DispatcherError::NotInitialized(String::new()));
        }
        Ok(())
    }
}

/// lib.rs:581-590
pub fn drive_index(key: CacheKey, num_drives: usize) -> usize {
    let mut h = key;
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 31;
    h as usize % num_drives
}

/// lib.rs:470-502
pub fn device_streams_for(c: &Ctx, device: i32) -> Option<DeviceStreams> {
    if device < 0 {
        return None;
    }
    if let Some((d, s)) = *c.device_streams.borrow() {
        if d == device {
            return Some(s);
        }
    }
    c.gpu.set_device(device).ok()?;
    let warm = c.gpu.create_stream().ok()?;
    let store_s = c.gpu.create_stream().ok()?;
    let pipe_a = c.gpu.create_stream().ok()?;
    let pipe_b = c.gpu.create_stream().ok()?;
    let entry = DeviceStreams {
        warm: warm.0 as u64,
        store: store_s.0 as u64,
        pipe: [pipe_a.0 as u64, pipe_b.0 as u64],
    };
    *c.device_streams.borrow_mut() = Some((device, entry));
    Some(entry)
}

/// lib.rs:508-516
pub fn set_batch_device(c: &Ctx, addr: *mut u8) -> i32 {
    match c.gpu.device_of_ptr(addr as *const std::ffi::c_void) {
        Ok(dev) if dev >= 0 => {
            let _ = c.gpu.set_device(dev);
            dev
        }
        _ => -1,
    }
}

/// lib.rs:572-579
pub fn emit_eviction(c: &Ctx, key: CacheKey, reason: EvictionReason) {
    if let Some(ref tx) = c.eviction_tx {
        if tx.try_send(EvictionEvent { key, reason }).is_err() {
            c.eviction_dropped.set(c.eviction_dropped.get() + 1);
        }
    }
}

/// lib.rs:1050-1108
pub fn evict_one_clean(c: &Ctx, scan: usize) -> bool {
    for cand in c.mt.oldest_keys(scan) {
        match c.dm.try_evict_to_block(cand) {
            Ok(()) => {
                let _ = c.mt.remove(cand);
                emit_eviction(c, cand, EvictionReason::Demoted);
                c.tier_counters.record_eviction_from_memory();
                return true;
            }
            Err(DispatchMapError::ActiveReferences(_)) => {
                c.tier_counters.record_eviction_blocked_by_pin();
            }
            Err(_) => {
                c.tier_counters.record_eviction_blocked_unpersisted();
            }
        }
    }
    c.tier_counters.record_eviction_scan_exhausted();
    false
}

/// lib.rs:1123-1168
pub fn evict_for_space(
    c: &Ctx,
    needed: u32,
    _target_key: CacheKey,
    max_attempts: usize,
) -> Result<(), DispatcherError> {
    const MAX_SCAN: usize = 4;
    let mut attempts = 0usize;
    while c.mt.used() + needed as usize > c.mt.capacity() {
        attempts += 1;
        if attempts > max_attempts {
            return Err(DispatcherError::AllocationFailed(String::new()));
        }
        let scan = (MAX_SCAN * attempts).min(1024);
        if !evict_one_clean(c, scan) {
            return Err(DispatcherError::AllocationFailed(String::new()));
        }
    }
    Ok(())
}

/// lib.rs:1176-1224
pub fn evict_and_insert(
    c: &Ctx,
    key: CacheKey,
    size: u32,
    max_attempts: usize,
) -> Result<*mut u8, DispatcherError> {
    const MAX_SCAN: usize = 4;
    let mut attempts = 0usize;
    loop {
        evict_for_space(c, size, key, max_attempts.saturating_sub(attempts))?;
        match c.mt.insert(key, size) {
            Ok(ptr) => return Ok(ptr),
            Err(MemoryTierError::PoolFull) => {
                attempts += 1;
                if attempts >= max_attempts {
                    return Err(DispatcherError::AllocationFailed(String::new()));
                }
                if !evict_one_clean(c, MAX_SCAN * attempts) {
                    return Err(DispatcherError::AllocationFailed(String::new()));
                }
            }
            Err(MemoryTierError::AlreadyExists(k)) => {
                c.tier_counters.record_store_already_resident();
                return Err(DispatcherError::AlreadyExists(k));
            }
            Err(_e) => {
                return Err(DispatcherError::AllocationFailed(String::new()));
            }
        }
    }
}

/// lib.rs:1262-1336
pub fn serve_memory_tier_to_gpu(
    c: &Ctx,
    pointer: *mut u8,
    size: u32,
    regions: &[IpcHandle],
    warm_raw: u64,
    synchronize: bool,
) -> Result<(), DispatcherError> {
    let raw = warm_raw;
    if raw != 0 {
        let s = GpuStream(raw as *mut std::ffi::c_void);
        let mut off: usize = 0;
        for region in regions {
            let copy_size = (region.size as usize).min((size as usize).saturating_sub(off));
            if copy_size == 0 {
                break;
            }
            c.gpu
                .memcpy_h2d_async(
                    pointer.wrapping_add(off) as *const std::ffi::c_void,
                    region.address as *mut std::ffi::c_void,
                    copy_size,
                    s,
                )
                .map_err(|_e| DispatcherError::IoError(String::new()))?;
            off += region.size as usize;
        }
        if synchronize {
            c.gpu
                .stream_synchronize(s)
                .map_err(|_e| DispatcherError::IoError(String::new()))?;
        }
        Ok(())
    } else {
        let mut off: usize = 0;
        for region in regions {
            let copy_size = (region.size as usize).min((size as usize).saturating_sub(off));
            if copy_size == 0 {
                break;
            }
            // DmaBuffer::from_raw(pointer + off, aligned, noop_free, -1): fails only on null.
            if pointer.is_null() && off == 0 {
                return Err(DispatcherError::IoError(String::new()));
            }
            let r = c
                .gpu
                .dma_copy_to_device(region.address as *mut std::ffi::c_void, copy_size)
                .map_err(|_e| DispatcherError::IoError(String::new()));
            r?;
            off += region.size as usize;
        }
        Ok(())
    }
}

/// lib.rs:1349-1388. The 5 s deadline (`start.elapsed() >= timeout`) is a
/// nondeterministic choice; the 50 us back-off sleep has no effect on state.
pub fn serve_concurrently_promoted(
    c: &Ctx,
    key: CacheKey,
    regions: &[IpcHandle],
    warm_raw: u64,
) -> Result<(), DispatcherError> {
    loop {
        match c.dm.lookup(key) {
            Ok(LookupResult::MemoryTier { pointer, size }) => {
                let res = serve_memory_tier_to_gpu(c, pointer, size, regions, warm_raw, true);
                let _ = c.dm.release_read(key);
                let _ = c.dm.touch(key);
                return res;
            }
            Ok(LookupResult::BlockDevice { .. }) => {
                let _ = c.dm.release_read(key);
                if nondet_bool() {
                    return Err(DispatcherError::KeyNotFound(key));
                }
            }
            Ok(LookupResult::MismatchSize) => {
                let _ = c.dm.release_read(key);
                return Err(DispatcherError::InvalidParameter(String::new()));
            }
            Ok(LookupResult::NotExist) => return Err(DispatcherError::KeyNotFound(key)),
            Err(_e) => return Err(DispatcherError::IoError(String::new())),
        }
    }
}

/// `pins::PinnedKeys` (pins.rs): releases every adopted read pin when dropped.
struct PinnedKeys<'a, 'c> {
    dm: &'a Dm<'c>,
    keys: Vec<CacheKey>,
}

impl<'a, 'c> PinnedKeys<'a, 'c> {
    fn new(dm: &'a Dm<'c>) -> Self {
        PinnedKeys { dm, keys: Vec::new() }
    }
    fn adopt(&mut self, key: CacheKey) {
        self.keys.push(key);
    }
}

impl Drop for PinnedKeys<'_, '_> {
    fn drop(&mut self) {
        for key in self.keys.drain(..) {
            let _ = self.dm.release_read(key);
        }
    }
}

pub struct ColdEntry {
    idx: usize,
    key: CacheKey,
    #[allow(dead_code)]
    offset: u64,
    #[allow(dead_code)]
    regions: Vec<IpcHandle>,
    total_size: u32,
}

/// lib.rs:2228-2992 (memory-tier-only mode: `data_drives` empty, so the cold block
/// takes the `num_drives == 0` branch at lib.rs:2481-2508). Receptacle-unbound early
/// returns (lib.rs:2233-2298) are omitted: every receptacle is bound in the harnesses.
pub fn batch_lookup(c: &Ctx, entries: &[(CacheKey, Vec<IpcHandle>)]) -> Vec<LookupOutcome> {
    if entries.is_empty() {
        return Vec::new();
    }
    let dm = &c.dm;
    let mt = &c.mt;

    let mut results: Vec<Option<Result<(), DispatcherError>>> = vec![None; entries.len()];
    let mut tier: Vec<Option<ServedBy>> = vec![None; entries.len()];
    let batch_device = entries
        .iter()
        .flat_map(|(_, regions)| regions.iter().map(|h| h.address))
        .find(|a| !a.is_null())
        .map_or(-1, |addr| set_batch_device(c, addr));
    let dev_streams = device_streams_for(c, batch_device);
    let warm_raw = dev_streams.as_ref().map_or(0, |s| s.warm);

    let mut cold_entries: Vec<ColdEntry> = Vec::new();
    let mut deferred_touch_keys: Vec<CacheKey> = Vec::new();
    let mut warm_pins = PinnedKeys::new(dm);

    for (i, (key, regions)) in entries.iter().enumerate() {
        let key = *key;
        let total_size: u32 = regions.iter().map(|r| r.size).sum();

        const LOOKUP_RETRIES: u32 = 5;
        let lookup = {
            let mut lk = dm.lookup(key);
            let mut retries = 0u32;
            while matches!(lk, Err(_)) && retries < LOOKUP_RETRIES {
                retries += 1;
                lk = dm.lookup(key);
            }
            lk
        };
        match lookup {
            Ok(lookup_result) => match lookup_result {
                LookupResult::NotExist => {
                    results[i] = Some(Err(DispatcherError::KeyNotFound(key)));
                }
                LookupResult::MismatchSize => {
                    let _ = dm.release_read(key);
                    results[i] = Some(Err(DispatcherError::InvalidParameter(String::new())));
                }
                LookupResult::MemoryTier { pointer, size } => {
                    let res = serve_memory_tier_to_gpu(c, pointer, size, regions, warm_raw, false);
                    warm_pins.adopt(key);
                    deferred_touch_keys.push(key);
                    tier[i] = Some(ServedBy::Dram);
                    results[i] = Some(res);
                }
                LookupResult::BlockDevice { offset } => {
                    let _ = dm.release_read(key);
                    tier[i] = Some(ServedBy::Ssd);
                    cold_entries.push(ColdEntry {
                        idx: i,
                        key,
                        offset,
                        regions: regions.clone(),
                        total_size,
                    });
                }
            },
            Err(_) => {
                results[i] = Some(Err(DispatcherError::KeyNotFound(key)));
            }
        }
    }

    if !deferred_touch_keys.is_empty() {
        mt.batch_touch(&deferred_touch_keys);
    }

    if !cold_entries.is_empty() {
        cold_no_drives(c, &cold_entries, &mut results);
    }

    if !deferred_touch_keys.is_empty() && warm_raw != 0 {
        let s = GpuStream(warm_raw as *mut std::ffi::c_void);
        if let Err(_e) = c.gpu.stream_synchronize(s) {
            c.log.info();
        }
    }
    drop(warm_pins);

    // --- Remote lookup for entries not found locally (lib.rs:2757-2935) ---
    if let Some(rl) = c.remote.as_ref() {
        remote_pass(c, rl, entries, &mut results, &mut tier, warm_raw);
    }

    // Recover concurrent-promotion losers (lib.rs:2941-2946).
    for (i, (key, regions)) in entries.iter().enumerate() {
        if matches!(results[i], Some(Err(DispatcherError::AlreadyExists(_)))) {
            results[i] = Some(serve_concurrently_promoted(c, *key, regions, warm_raw));
        }
    }

    // Attribution (lib.rs:2952-2977).
    let out: Vec<LookupOutcome> = results
        .into_iter()
        .zip(tier)
        .map(|(r, t)| {
            let result = r.unwrap();
            let served_by = match (&result, t) {
                (Ok(()), Some(t)) => t,
                (Ok(()), None) => ServedBy::Error,
                (Err(DispatcherError::KeyNotFound(_)), _) => ServedBy::Miss,
                (Err(DispatcherError::InvalidParameter(_)), _) => ServedBy::SizeMismatch,
                (Err(_), _) => ServedBy::Error,
            };
            LookupOutcome { served_by, result }
        })
        .collect();
    let served = out.iter().filter(|r| r.result.is_ok()).count() as u64;
    if served > 0 {
        c.tier_counters.record_promotions_to_gpu(served);
    }
    let dram = out.iter().filter(|o| o.served_by == ServedBy::Dram).count() as u64;
    let ssd = out.iter().filter(|o| o.served_by == ServedBy::Ssd).count() as u64;
    c.tier_counters.record_local_hits(dram, ssd);
    out
}

/// lib.rs:2472-2508 (`if !cold_entries.is_empty()` body, memory-tier-only branch),
/// moved out of [`batch_lookup`] unchanged so a harness can guard it.
fn cold_no_drives(
    c: &Ctx,
    cold_entries: &[ColdEntry],
    results: &mut [Option<Result<(), DispatcherError>>],
) {
    let dm = &c.dm;
    let mt = &c.mt;
    let num_drives = c.data_drives.borrow().len();
    // Drive-backed branch (lib.rs:2509-2739) not mirrored; see module docs.
    assert!(num_drives == 0, "mirror covers the memory-tier-only branch only");
    let max_attempts = c.max_eviction_attempts;
    for entry in cold_entries.iter() {
        evict_for_space(c, entry.total_size, entry.key, max_attempts).ok();
        let res = mt
            .insert(entry.key, entry.total_size)
            .map(|mem_ptr| {
                if dm
                    .promote_block_to_memory_tier(entry.key, mem_ptr, entry.total_size)
                    .is_ok()
                {
                    c.tier_counters.record_promotion_to_memory();
                }
            })
            .map_err(|e| match e {
                MemoryTierError::AlreadyExists(k) => DispatcherError::AlreadyExists(k),
                _other => DispatcherError::AllocationFailed(String::new()),
            });
        results[entry.idx] = Some(res);
    }
}

/// lib.rs:2757-2935 (`if let Ok(rl) = self.remote_lookup.get()` body), moved out of
/// [`batch_lookup`] unchanged so a harness can guard it.
fn remote_pass(
    c: &Ctx,
    rl: &Remote,
    entries: &[(CacheKey, Vec<IpcHandle>)],
    results: &mut [Option<Result<(), DispatcherError>>],
    tier: &mut [Option<ServedBy>],
    warm_raw: u64,
) {
    let dm = &c.dm;
    let not_found: Vec<usize> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Some(Err(DispatcherError::KeyNotFound(_))) => Some(i),
            _ => None,
        })
        .collect();

    if !not_found.is_empty() {
        let remote_entries: Vec<(CacheKey, u32)> = not_found
            .iter()
            .map(|&i| {
                let (key, regions) = &entries[i];
                (*key, regions.iter().map(|r| r.size).sum())
            })
            .collect();
        let remote_results = rl.batch_lookup(&remote_entries);
        let mut remote_pins = PinnedKeys::new(dm);
        let mut submitted: Vec<usize> = Vec::with_capacity(not_found.len());
        let mut remote_hits: u64 = 0;
        let mut remote_misses: u64 = 0;

        for (&pos, remote_res) in not_found.iter().zip(remote_results.into_iter()) {
            let (key, regions) = &entries[pos];
            let key = *key;
            if let Err(e) = remote_res {
                if matches!(e, interfaces::RemoteLookupError::NotFound) {
                    remote_misses += 1;
                }
                results[pos] = Some(Err(match e {
                    interfaces::RemoteLookupError::NotFound => {
                        DispatcherError::KeyNotFound(key)
                    }
                    _other => DispatcherError::IoError(String::new()),
                }));
                continue;
            }
            remote_hits += 1;
            let looked_up = dm.lookup(key);
            match looked_up {
                Ok(LookupResult::MemoryTier { pointer, size }) => {
                    remote_pins.adopt(key);
                    let res =
                        serve_memory_tier_to_gpu(c, pointer, size, regions, warm_raw, false);
                    if res.is_ok() {
                        submitted.push(pos);
                    }
                    tier[pos] = Some(ServedBy::Remote);
                    results[pos] = Some(res);
                }
                Ok(LookupResult::BlockDevice { .. }) => {
                    let _ = dm.release_read(key);
                    results[pos] = Some(Err(DispatcherError::KeyNotFound(key)));
                }
                Ok(_) => {
                    results[pos] = Some(Err(DispatcherError::KeyNotFound(key)));
                }
                Err(_e) => {
                    results[pos] = Some(Err(DispatcherError::IoError(String::new())));
                }
            }
        }

        if !submitted.is_empty() && warm_raw != 0 {
            let s = GpuStream(warm_raw as *mut std::ffi::c_void);
            if let Err(_e) = c.gpu.stream_synchronize(s) {
                c.log.warn();
                for &pos in &submitted {
                    results[pos] = Some(Err(DispatcherError::IoError(String::new())));
                }
            }
        }
        drop(remote_pins);
        c.tier_counters.record_remote_lookup(remote_hits, remote_misses);
    }
}

/// lib.rs:750-819, the memory-tier-only branch (`drives.is_empty()`); the staging
/// fallback needs drives (`has_drives`) and is not taken.
pub fn promote_and_serve(
    c: &Ctx,
    key: CacheKey,
    _offset: u64,
    ipc_handle: &IpcHandle,
    _device: i32,
) -> Result<(), DispatcherError> {
    let total_bytes = ipc_handle.size as usize;
    let max_attempts = c.max_eviction_attempts;
    let has_drives = !c.data_drives.borrow().is_empty();
    assert!(!has_drives, "mirror covers the memory-tier-only branch only");
    let mem_ptr = evict_and_insert(c, key, ipc_handle.size, max_attempts)?;

    // drives.is_empty(): "No hardware: just copy zeros to GPU" (lib.rs:795-819)
    if mem_ptr.is_null() {
        return Err(DispatcherError::IoError(String::new()));
    }
    let result = c
        .gpu
        .dma_copy_to_device(ipc_handle.address as *mut std::ffi::c_void, total_bytes);
    if c.dm.create_memory_tier_entry(key, mem_ptr, ipc_handle.size).is_ok() {
        c.tier_counters.record_promotion_to_memory();
    }
    let _ = c.dm.release_write(key);
    result.map_err(|_e| DispatcherError::IoError(String::new()))
}

/// lib.rs:2994-3073
pub fn lookup_async(c: &Ctx, key: CacheKey, ipc_handle: IpcHandle) -> Result<GpuStream, DispatcherError> {
    c.ensure_initialized()?;
    let result = c.dm.lookup(key);
    let null_stream = GpuStream(std::ptr::null_mut());
    let device = set_batch_device(c, ipc_handle.address);
    let warm_raw = device_streams_for(c, device).map_or(0, |s| s.warm);

    match result {
        Ok(lookup_result) => match lookup_result {
            LookupResult::NotExist => Err(DispatcherError::KeyNotFound(key)),
            LookupResult::MismatchSize => {
                let _ = c.dm.release_read(key);
                Err(DispatcherError::InvalidParameter(String::new()))
            }
            LookupResult::MemoryTier { pointer, size } => {
                let r = serve_memory_tier_to_gpu(
                    c,
                    pointer,
                    size,
                    std::slice::from_ref(&ipc_handle),
                    warm_raw,
                    true,
                );
                let _ = c.dm.release_read(key);
                c.mt.touch(key);
                if r.is_ok() {
                    c.tier_counters.record_promotion_to_gpu();
                }
                r.map(|()| null_stream)
            }
            LookupResult::BlockDevice { offset } => {
                let _ = c.dm.release_read(key);
                promote_and_serve(c, key, offset, &ipc_handle, device)?;
                c.tier_counters.record_promotion_to_gpu();
                Ok(null_stream)
            }
        },
        Err(_) => Err(DispatcherError::KeyNotFound(key)),
    }
}

/// lib.rs:2197-2208
pub fn lookup(c: &Ctx, key: CacheKey, ipc_handle: IpcHandle) -> Result<(), DispatcherError> {
    let stream = lookup_async(c, key, ipc_handle)?;
    if !stream.0.is_null() {
        c.gpu
            .stream_synchronize(stream)
            .map_err(|_e| DispatcherError::IoError(String::new()))?;
    }
    Ok(())
}

/// lib.rs:3096-3136
pub fn remove(c: &Ctx, key: CacheKey) -> Result<(), DispatcherError> {
    c.ensure_initialized()?;
    let block_offset = match c.dm.lookup(key) {
        Ok(LookupResult::BlockDevice { offset }) => {
            let _ = c.dm.release_read(key);
            Some(offset)
        }
        Ok(_) => {
            let _ = c.dm.release_read(key);
            None
        }
        Err(_) => return Err(DispatcherError::KeyNotFound(key)),
    };
    c.dm.remove(key).map_err(|_| DispatcherError::KeyNotFound(key))?;
    let _ = c.mt.remove(key);
    if let Some(offset) = block_offset {
        let drives = c.data_drives.borrow();
        let idx = drive_index(key, drives.len().max(1));
        if let Some(drive) = drives.get(idx) {
            let _ = drive.extents.remove_extent(offset);
        }
    }
    Ok(())
}

/// lib.rs:3318-3420 with the per-call budget path (`deadline == None`). With
/// `store_backpressure_ms == 0` it is a single `evict_and_insert` (lib.rs:3391-3394);
/// otherwise each retry is taken while a nondeterministic clock is before the deadline.
pub fn reserve_memory(c: &Ctx, key: CacheKey, size: u32) -> Result<*mut u8, DispatcherError> {
    c.ensure_initialized()?;
    if size == 0 {
        return Err(DispatcherError::InvalidParameter(String::new()));
    }
    let max_attempts = c.max_eviction_attempts;
    if c.store_backpressure_ms == 0 {
        let mem_ptr = evict_and_insert(c, key, size, max_attempts)?;
        return Ok(mem_ptr);
    }
    loop {
        match evict_and_insert(c, key, size, max_attempts) {
            Ok(mem_ptr) => return Ok(mem_ptr),
            Err(DispatcherError::AllocationFailed(_)) if nondet_bool() => {
                if c.tier_counters.record_store_backpressure() == 0 {
                    c.log.info();
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// lib.rs:3794-3825
pub fn clear_memory_tier(c: &Ctx) -> Result<usize, DispatcherError> {
    c.ensure_initialized()?;
    let mut count = 0;
    loop {
        let candidates = c.mt.oldest_keys(64);
        if candidates.is_empty() {
            break;
        }
        for key in candidates {
            if c.dm.try_evict_to_block(key).is_ok() {
                let _ = c.mt.remove(key);
            } else {
                let _ = c.mt.remove(key);
                let _ = c.dm.remove(key);
            }
            count += 1;
        }
    }
    Ok(count)
}

/// lib.rs:3827-3840 with `ParallelBackgroundWriter::{in_flight, flush}`
/// (background.rs:193-205): returns once every queue is drained.
pub fn flush_to_ssd(c: &Ctx) -> Result<usize, DispatcherError> {
    c.ensure_initialized()?;
    let flushed = if let Some(ref in_flight) = c.bg_writer_in_flight {
        let before = in_flight.get();
        // writer.flush(): loop { if all in_flight == 0 { return } sleep(5ms) }
        assert!(in_flight.get() == 0, "flush returns only once the queue is drained");
        before
    } else {
        0
    };
    Ok(flushed)
}

/// One write-through job (background.rs:117-120 `worker_loop` + lib.rs:1390-1466
/// `process_write_job`) for a configuration with one data drive.
pub fn process_write_job(c: &Ctx, key: CacheKey, _size: u32) {
    let (_mem_ptr, _size) = match c.mt.peek(key) {
        Some(v) => v,
        None => {
            let _ = c.dm.release_read(key);
            finish_job(c);
            return;
        }
    };
    let drives = c.data_drives.borrow();
    if drives.is_empty() {
        let block_offset = key * 4096;
        let _ = c.dm.convert_to_storage(key, block_offset);
        finish_job(c);
        return;
    }
    let drive = &drives[0];
    // iem.reserve_extent(...) (lib.rs:1436-1443)
    let block_offset = match drive.extents.reserve_extent() {
        Ok(o) => o,
        Err(_) => {
            let _ = c.dm.release_read(key);
            finish_job(c);
            return;
        }
    };
    // write_buffer_to_ssd succeeded; publish + convert_to_storage (lib.rs:1454-1465)
    let _ = c.dm.convert_to_storage(key, block_offset);
    finish_job(c);
}

fn finish_job(c: &Ctx) {
    if let Some(ref f) = c.bg_writer_in_flight {
        f.set(f.get() - 1);
    }
}

/// lib.rs:3866-3876
pub fn read_write_stats(c: &Ctx) -> ReadWriteStats {
    let mut agg = ReadWriteStats::default();
    for drive in c.data_drives.borrow().iter() {
        agg.merge_from(&drive.rw_stats);
    }
    agg
}

/// The effect of `shutdown` (lib.rs:2112-2195) on the state `read_write_stats` reads:
/// `data_drives` is taken (lib.rs:2161-2164) and `initialized` cleared (lib.rs:2192).
pub fn shutdown(c: &Ctx) -> Result<(), DispatcherError> {
    let drives = std::mem::take(&mut *c.data_drives.borrow_mut());
    drop(drives);
    c.initialized.set(false);
    Ok(())
}

/// lib.rs:1884-2110, the part between the receptacle checks and the background
/// workers: drive creation (`create_data_drives`, assumed to succeed), warm-stream
/// allocation and the memory-tier pool registration (lib.rs:1906-2001). The
/// recovery pass is skipped (`format_on_init` defaults to true) and
/// `PipelineRing::new` does not affect the registration step.
pub fn initialize(c: &Ctx, spdk_env_connected: bool, has_bd_factory: bool) -> Result<(), DispatcherError> {
    c.log.info(); // "dispatcher: initializing"
    if spdk_env_connected || has_bd_factory {
        // *self.data_drives.write() = create_data_drives(&config)?;  (one drive)
        c.data_drives.borrow_mut().push(Drive {
            extents: Extents::default(),
            rw_stats: ReadWriteStats::default(),
        });
        // gpu_services bound:
        match c.gpu.create_stream() {
            Ok(stream) => c.warm_stream.set(stream.0 as u64),
            Err(_e) => c.log.info(),
        }
        if spdk_env_connected {
            if let Some((pool_ptr, pool_size)) = c.mt.pool_info() {
                match c.gpu.register_host_memory(pool_ptr as *mut std::ffi::c_void, pool_size) {
                    Ok(()) => c.log.info(),
                    Err(_e) => c.log.info(), // "memory-tier pool registration failed (non-fatal)"
                }
            }
        }
    }
    c.initialized.set(true);
    c.log.info(); // "dispatcher: initialized"
    Ok(())
}

/// One pass of `MemoryTierEvictor::evictor_loop` (background.rs:466-560) once
/// utilization is at or above the threshold: demote the scanned candidates until the
/// low watermark. Logging and the pacing sleep are omitted.
pub fn mt_evictor_pass(c: &Ctx, scan_size: usize, low_watermark: f64) -> u32 {
    let capacity = c.mt.capacity();
    let candidates = c.mt.oldest_keys(scan_size);
    let mut demoted = 0u32;
    for key in candidates {
        if c.dm.try_evict_to_block(key).is_err() {
            continue;
        }
        if c.mt.remove(key).is_err() {
            continue;
        }
        if let Some(ref tx) = c.eviction_tx {
            let _ = tx.try_send(EvictionEvent { key, reason: EvictionReason::Demoted });
        }
        demoted += 1;
        c.tier_counters.record_eviction_from_memory();
        let used_now = c.mt.used();
        let util_now = used_now as f64 / capacity as f64;
        if util_now < low_watermark {
            break;
        }
    }
    demoted
}

// ---------------------------------------------------------------------------
// Cold-read pipelines (pipeline.rs)
// ---------------------------------------------------------------------------

/// The completion kinds the cold-read pipelines distinguish (`interfaces::Completion`
/// matched at pipeline.rs:572-696 / 968-1107): `ReadDone { tag, result }`,
/// `Timeout { .. }`, any other completion, or a closed channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cpl {
    ReadDone { tag: u64, ok: bool },
    Timeout,
    Other,
    Closed,
}

/// A block-device client channel pair (`interfaces::ClientChannels`): the reads the
/// pipeline submitted, and the completions the drive actor delivers, in order.
pub struct Chan {
    /// (lba, tag) of every `Command::ReadAsync` sent.
    pub sent: RefCell<Vec<(u64, u64)>>,
    pub completions: [Cpl; 4],
    pub next: Cell<usize>,
    /// Number of completions delivered when the pipeline returned.
    pub received: Cell<usize>,
}

impl Chan {
    pub fn new(completions: [Cpl; 4]) -> Self {
        Chan {
            sent: RefCell::new(Vec::new()),
            completions,
            next: Cell::new(0),
            received: Cell::new(0),
        }
    }
    fn send(&self, lba: u64, tag: u64) -> Result<(), ()> {
        self.sent.borrow_mut().push((lba, tag));
        Ok(())
    }
    /// `completion_rx.recv()` / `try_recv()`: the next scripted completion.
    fn recv(&self) -> Cpl {
        let i = self.next.get();
        assert!(i < self.completions.len(), "model: completion script exhausted");
        self.next.set(i + 1);
        let c = self.completions[i];
        if c != Cpl::Closed {
            self.received.set(self.received.get() + 1);
        }
        c
    }
}

/// pipeline.rs:437-442
#[derive(Clone, Copy)]
pub struct ColdReadJob {
    pub mem_ptr: *mut u8,
    pub gpu_dst: *mut std::ffi::c_void,
    pub start_lba: u64,
    pub total_bytes: usize,
}

/// pipeline.rs:457-743 (`pipelined_multi_object_zero_copy`); the per-segment
/// `DmaBuffer` wrappers and timing are omitted. Segmentation is the REAL
/// `dispatcher::io_segmenter::segment_io`.
pub fn pipelined_multi_object_zero_copy(
    block_size: u32,
    gpu: &Gpu,
    streams: &[GpuStream; 2],
    channels: &Chan,
    jobs: &[ColdReadJob],
    chunk_size: usize,
    max_queue_depth: usize,
) -> Vec<Result<(), DispatcherError>> {
    let block_size = block_size as usize;
    let num_jobs = jobs.len();
    let mut results: Vec<Result<(), DispatcherError>> = vec![Ok(()); num_jobs];
    if num_jobs == 0 {
        return results;
    }
    let mut all_objs: Vec<Vec<dispatcher::io_segmenter::IoSegment>> = Vec::with_capacity(num_jobs);
    let mut total_segments = 0usize;
    for job in jobs {
        let aligned_bytes = job.total_bytes.next_multiple_of(block_size);
        let segments = dispatcher::io_segmenter::segment_io(
            job.start_lba,
            aligned_bytes,
            chunk_size as u32,
            block_size as u32,
        );
        total_segments += segments.len();
        all_objs.push(segments);
    }
    if total_segments == 0 {
        return results;
    }
    let mut work: Vec<(usize, usize)> = Vec::with_capacity(total_segments);
    for (obj_idx, obj) in all_objs.iter().enumerate() {
        for seg_idx in 0..obj.len() {
            work.push((obj_idx, seg_idx));
        }
    }
    let max_segments_per_obj = all_objs.iter().map(|o| o.len()).max().unwrap_or(0);

    let submit_limit = max_queue_depth.min(work.len());
    let mut submitted = 0usize;
    let mut completed = 0usize;
    let mut stream_idx = 0usize;
    while submitted < submit_limit {
        let (obj_idx, seg_idx) = work[submitted];
        let tag = (obj_idx * max_segments_per_obj + seg_idx) as u64;
        if channels.send(all_objs[obj_idx][seg_idx].lba, tag).is_err() {
            for result in results.iter_mut().take(num_jobs) {
                *result = Err(DispatcherError::IoError(String::new()));
            }
            return results;
        }
        submitted += 1;
    }

    let mut obj_remaining: Vec<usize> = all_objs.iter().map(|o| o.len()).collect();
    let mut obj_failed: Vec<bool> = vec![false; num_jobs];
    let mut stop_submitting = false;
    while completed < submitted {
        match channels.recv() {
            Cpl::ReadDone { tag, ok } => {
                completed += 1;
                let obj_idx = (tag as usize) / max_segments_per_obj;
                let seg_idx = (tag as usize) % max_segments_per_obj;
                if obj_idx >= num_jobs || seg_idx >= all_objs.get(obj_idx).map_or(0, |o| o.len()) {
                    for r in results.iter_mut() {
                        if r.is_ok() {
                            *r = Err(DispatcherError::IoError(String::new()));
                        }
                    }
                    obj_failed.fill(true);
                    stop_submitting = true;
                } else if !ok {
                    results[obj_idx] = Err(DispatcherError::IoError(String::new()));
                    obj_failed[obj_idx] = true;
                    obj_remaining[obj_idx] = obj_remaining[obj_idx].saturating_sub(1);
                } else {
                    obj_remaining[obj_idx] -= 1;
                }
                if !stop_submitting && submitted < work.len() {
                    let (next_obj, next_seg) = work[submitted];
                    let next_tag = (next_obj * max_segments_per_obj + next_seg) as u64;
                    if channels.send(all_objs[next_obj][next_seg].lba, next_tag).is_ok() {
                        submitted += 1;
                    } else {
                        stop_submitting = true;
                    }
                }
                if obj_idx < num_jobs
                    && obj_remaining[obj_idx] == 0
                    && !obj_failed[obj_idx]
                    && !jobs[obj_idx].gpu_dst.is_null()
                {
                    let job = &jobs[obj_idx];
                    let current_stream = streams[stream_idx % 2];
                    let dma_result = gpu.memcpy_h2d_async(
                        job.mem_ptr as *const std::ffi::c_void,
                        job.gpu_dst,
                        job.total_bytes,
                        current_stream,
                    );
                    if let Err(_e) = dma_result {
                        results[obj_idx] = Err(DispatcherError::IoError(String::new()));
                    }
                    stream_idx += 1;
                    if stream_idx % dispatcher::pipeline::PIPELINE_RING_SIZE == 0 {
                        let _ = gpu.stream_synchronize(streams[0]);
                        let _ = gpu.stream_synchronize(streams[1]);
                    }
                }
            }
            Cpl::Timeout => {
                for r in results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                stop_submitting = true;
                completed += 1;
            }
            Cpl::Other => {
                for r in results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                stop_submitting = true;
                completed += 1;
            }
            Cpl::Closed => {
                for r in results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                break;
            }
        }
    }
    for &(obj_idx, _seg_idx) in work.iter().skip(submitted) {
        if results[obj_idx].is_ok() {
            results[obj_idx] = Err(DispatcherError::IoError(String::new()));
        }
    }
    let _ = gpu.stream_synchronize(streams[0]);
    let _ = gpu.stream_synchronize(streams[1]);
    results
}

/// pipeline.rs:771-1145 (`scatter_gather_multi_drive_zero_copy`) for ONE drive (the
/// per-drive queue-depth split, lib.rs:853-875, then gives that drive the whole
/// budget). With one drive `try_recv` on a non-empty queue behaves as `recv`; the
/// spin/yield back-off on an empty queue changes no state.
pub fn scatter_gather_one_drive(
    block_size: u32,
    gpu: &Gpu,
    streams: &[GpuStream; 2],
    channels: &Chan,
    jobs: &[ColdReadJob],
    chunk_size: usize,
    max_queue_depth: usize,
) -> Vec<Result<(), DispatcherError>> {
    let block_size = block_size as usize;
    let mut all_results: Vec<Result<(), DispatcherError>> = vec![Ok(()); jobs.len()];
    let mut objs: Vec<Vec<dispatcher::io_segmenter::IoSegment>> = Vec::with_capacity(jobs.len());
    let mut total_segments = 0usize;
    let mut work: Vec<(usize, usize)> = Vec::new();
    for (obj_idx, job) in jobs.iter().enumerate() {
        let aligned_bytes = job.total_bytes.next_multiple_of(block_size);
        let segments = dispatcher::io_segmenter::segment_io(
            job.start_lba,
            aligned_bytes,
            chunk_size as u32,
            block_size as u32,
        );
        for seg_idx in 0..segments.len() {
            work.push((obj_idx, seg_idx));
        }
        total_segments += segments.len();
        objs.push(segments);
    }
    let max_segments_per_obj = objs.iter().map(|o| o.len()).max().unwrap_or(0);
    if total_segments == 0 {
        return all_results;
    }
    // per_drive_qd with one drive: share = max_queue_depth, .max(1).min(work.len())
    let qd = (max_queue_depth as u64 * total_segments as u64 / total_segments as u64) as usize;
    let limit = qd.max(1).min(work.len());

    let mut submitted = 0usize;
    let mut completed = 0usize;
    let mut stop_submitting = false;
    let mut obj_remaining: Vec<usize> = objs.iter().map(|o| o.len()).collect();
    let mut obj_failed: Vec<bool> = vec![false; jobs.len()];
    let mut stream_idx: usize = 0;

    while submitted < limit {
        let (obj_idx, seg_idx) = work[submitted];
        let tag = (obj_idx * max_segments_per_obj + seg_idx) as u64;
        if channels.send(objs[obj_idx][seg_idx].lba, tag).is_err() {
            for r in all_results.iter_mut() {
                *r = Err(DispatcherError::IoError(String::new()));
            }
            stop_submitting = true;
            break;
        }
        submitted += 1;
    }

    while completed < submitted {
        match channels.recv() {
            Cpl::ReadDone { tag, ok } => {
                completed += 1;
                let obj_idx = (tag as usize) / max_segments_per_obj;
                let seg_idx = (tag as usize) % max_segments_per_obj;
                if obj_idx >= jobs.len() || seg_idx >= objs.get(obj_idx).map_or(0, |o| o.len()) {
                    for r in all_results.iter_mut() {
                        if r.is_ok() {
                            *r = Err(DispatcherError::IoError(String::new()));
                        }
                    }
                    obj_failed.fill(true);
                    stop_submitting = true;
                } else if !ok {
                    all_results[obj_idx] = Err(DispatcherError::IoError(String::new()));
                    obj_failed[obj_idx] = true;
                    obj_remaining[obj_idx] = obj_remaining[obj_idx].saturating_sub(1);
                } else {
                    obj_remaining[obj_idx] -= 1;
                }
                if !stop_submitting && submitted < work.len() {
                    let (next_obj, next_seg) = work[submitted];
                    let next_tag = (next_obj * max_segments_per_obj + next_seg) as u64;
                    if channels.send(objs[next_obj][next_seg].lba, next_tag).is_ok() {
                        submitted += 1;
                    } else {
                        stop_submitting = true;
                    }
                }
                if obj_idx < jobs.len()
                    && obj_remaining[obj_idx] == 0
                    && !obj_failed[obj_idx]
                    && !jobs[obj_idx].gpu_dst.is_null()
                {
                    let job = &jobs[obj_idx];
                    let current_stream = streams[stream_idx % 2];
                    let dma_result = gpu.memcpy_h2d_async(
                        job.mem_ptr as *const std::ffi::c_void,
                        job.gpu_dst,
                        job.total_bytes,
                        current_stream,
                    );
                    if let Err(_e) = dma_result {
                        all_results[obj_idx] = Err(DispatcherError::IoError(String::new()));
                    }
                    stream_idx += 1;
                }
            }
            Cpl::Timeout => {
                completed += 1;
                for r in all_results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                stop_submitting = true;
            }
            Cpl::Other => {
                completed += 1;
                for r in all_results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                stop_submitting = true;
            }
            Cpl::Closed => {
                for r in all_results.iter_mut() {
                    if r.is_ok() {
                        *r = Err(DispatcherError::IoError(String::new()));
                    }
                }
                obj_failed.fill(true);
                stop_submitting = true;
                completed = submitted;
            }
        }
    }
    let _ = stop_submitting;
    for &(obj_idx, _) in work.iter().skip(submitted) {
        if all_results[obj_idx].is_ok() {
            all_results[obj_idx] = Err(DispatcherError::IoError(String::new()));
        }
    }
    let _ = gpu.stream_synchronize(streams[0]);
    let _ = gpu.stream_synchronize(streams[1]);
    all_results
}

/// A failed NVMe read completion.
pub fn read_failed(tag: u64) -> Cpl {
    Cpl::ReadDone { tag, ok: false }
}

/// A successful NVMe read completion.
pub fn read_ok(tag: u64) -> Cpl {
    Cpl::ReadDone { tag, ok: true }
}

/// Guards for paths a harness does not take. A harness that stubs a mirror function
/// with one of these PANICS if that function is ever reached, so the stub can only
/// make a harness fail, never pass: a passing harness shows the stubbed path is
/// unreachable from its start state, i.e. its executions are those of the unstubbed
/// mirror. (Kani cannot always constant-fold the niche-encoded enum discriminants, so
/// dead arms otherwise stay in the SAT formula and blow the time budget.)
pub mod not_taken {
    use super::*;

    pub fn cold_no_drives(
        _c: &Ctx,
        _cold: &[ColdEntry],
        _results: &mut [Option<Result<(), DispatcherError>>],
    ) {
        panic!("path not taken: cold_no_drives")
    }
    pub fn remote_pass(
        _c: &Ctx,
        _rl: &Remote,
        _entries: &[(CacheKey, Vec<IpcHandle>)],
        _results: &mut [Option<Result<(), DispatcherError>>],
        _tier: &mut [Option<ServedBy>],
        _warm_raw: u64,
    ) {
        panic!("path not taken: remote_pass")
    }

    pub fn evict_one_clean(_c: &Ctx, _scan: usize) -> bool {
        panic!("path not taken: evict_one_clean")
    }
    pub fn promote_and_serve(
        _c: &Ctx,
        _key: CacheKey,
        _offset: u64,
        _h: &IpcHandle,
        _device: i32,
    ) -> Result<(), DispatcherError> {
        panic!("path not taken: promote_and_serve")
    }
    pub fn serve_concurrently_promoted(
        _c: &Ctx,
        _key: CacheKey,
        _regions: &[IpcHandle],
        _warm_raw: u64,
    ) -> Result<(), DispatcherError> {
        panic!("path not taken: serve_concurrently_promoted")
    }
    pub fn serve_memory_tier_to_gpu(
        _c: &Ctx,
        _pointer: *mut u8,
        _size: u32,
        _regions: &[IpcHandle],
        _warm_raw: u64,
        _synchronize: bool,
    ) -> Result<(), DispatcherError> {
        panic!("path not taken: serve_memory_tier_to_gpu")
    }
}
