//! Mirror of the `DispatcherComponent` methods in `src/lib.rs` that the loom
//! targets exercise. The GPU side is a slot copy into a returned `u64`; the
//! dispatcher's statistics counters, metrics and logging are dropped.

use crate::background::{
    EvictionEvent, EvictionReason, EvictionSender, JobStart, ParallelBackgroundWriter, WriteJob,
    WriteThroughPins,
};
use crate::dmap::{DispatchMap, DmError, LookupResult};
use crate::stand::{Chan, ExtentManager, MemoryTier, MtError};
use crate::{CacheKey, Mutation};
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// `interfaces::DispatcherError` (payloads dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DErr {
    KeyNotFound(CacheKey),
    InvalidParameter,
    AllocationFailed,
    AlreadyExists(CacheKey),
    IoError,
}

/// `interfaces::ServedBy`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedBy {
    Dram,
    Ssd,
    Remote,
    Miss,
    SizeMismatch,
    Error,
}

/// `interfaces::LookupOutcome`, plus observation fields for the models.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub served_by: ServedBy,
    pub result: Result<(), DErr>,
    /// The result was overwritten by the concurrent-promotion recovery pass.
    pub overwritten: bool,
    /// The recorded tier was rewritten in that same step.
    pub tier_rewritten: bool,
}

/// Points at which a model can observe or rendezvous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// Inside a lookup's read section (read reference held, slot being served).
    ReadSection(CacheKey),
}

pub type HookFn = Box<dyn Fn(Hook) + Send + Sync>;

/// One cold entry of batch_lookup's pooled path: `idx` is the result slot,
/// plus key, size and SSD offset (src/lib.rs:2591 DriveJobSet's job).
struct DriveJob {
    idx: usize,
    key: CacheKey,
    size: u32,
    offset: u64,
}

/// src/lib.rs:2591 (DriveJobSet): one drive's cold jobs and their slots.
struct DriveJobSet {
    drive_idx: usize,
    jobs: Vec<DriveJob>,
    mem_ptrs: Vec<usize>,
}

// mirrors src/lib.rs:587 (drive_index). Every model has at most one drive, so
// the splitmix64 placement hash is not needed.
pub fn drive_index(key: CacheKey, num_drives: usize) -> usize {
    (key as usize) % num_drives
}

pub struct Dispatcher {
    pub dm: Arc<DispatchMap>,
    pub mt: Arc<MemoryTier>,
    /// `data_drives[i].extent_mgr` (empty = no-hardware mode).
    pub drives: Vec<Arc<ExtentManager>>,
    pub bg_writer: Mutex<Option<ParallelBackgroundWriter>>,
    pub eviction_tx: Arc<Mutex<Option<EvictionSender>>>,
    pub eviction_dropped: Arc<AtomicU64>,
    pub max_eviction_attempts: usize,
    /// `store_backpressure_ms > 0`
    pub store_backpressure: bool,
    /// IGpuServices boundary stand-in: `gpu.create_stream()` fails (the
    /// scatter-gather stream setup in batch_lookup, src/lib.rs:2670).
    pub gpu_fail_create_stream: bool,
    pub hook: Option<HookFn>,
    pub write_through_pins: Arc<WriteThroughPins>,
    pub m: Mutation,
}

impl Dispatcher {
    pub fn new(dm: Arc<DispatchMap>, mt: Arc<MemoryTier>, drives: Vec<Arc<ExtentManager>>, m: Mutation) -> Self {
        Self {
            dm,
            mt,
            drives,
            bg_writer: Mutex::new(None),
            eviction_tx: Arc::new(Mutex::new(None)),
            eviction_dropped: Arc::new(AtomicU64::new(0)),
            max_eviction_attempts: 4,
            store_backpressure: true,
            gpu_fail_create_stream: false,
            hook: None,
            write_through_pins: Arc::new(WriteThroughPins::default()),
            m,
        }
    }

    // mirrors src/lib.rs:2083 (initialize: ParallelBackgroundWriter::start)
    pub fn start_bg_writer(&self) {
        let n = self.drives.len().max(1);
        let (dm, mt, drives) = (self.dm.clone(), self.mt.clone(), self.drives.clone());
        let pins = self.write_through_pins.clone();
        let w = ParallelBackgroundWriter::start(
            n,
            move |_idx| {
                let (pins, dm, mt, drives) = (pins.clone(), dm.clone(), mt.clone(), drives.clone());
                move |job| Self::run_write_job(&pins, &dm, &mt, &drives, job)
            },
            self.m,
        );
        *self.bg_writer.lock().unwrap() = Some(w);
    }

    // mirrors src/lib.rs:2172 (shutdown: bg_writer part)
    pub fn shutdown(&self) {
        if let Some(mut w) = self.bg_writer.lock().unwrap().take() {
            w.shutdown();
        }
    }

    // mirrors src/lib.rs:563 (create_eviction_channel)
    pub fn create_eviction_channel(&self, capacity: usize) -> Arc<Chan<EvictionEvent>> {
        let ch = Arc::new(Chan::bounded(capacity));
        *self.eviction_tx.lock().unwrap() =
            Some(EvictionSender { tx: ch.clone(), dropped: Arc::clone(&self.eviction_dropped) });
        ch
    }

    // mirrors src/lib.rs:576 (eviction_dropped_count)
    pub fn eviction_dropped_count(&self) -> u64 {
        self.eviction_dropped.swap(0, Ordering::Relaxed)
    }

    // mirrors src/lib.rs:580 (emit_eviction)
    fn emit_eviction(&self, key: CacheKey, reason: EvictionReason) {
        let guard = self.eviction_tx.lock().unwrap();
        if let Some(ref tx) = *guard {
            tx.emit(EvictionEvent { key, reason });
        }
    }

    // mirrors src/lib.rs:1056 (evict_one_clean)
    fn evict_one_clean(&self, scan: usize) -> bool {
        for cand in self.mt.oldest_keys(scan) {
            match self.dm.try_evict_to_block(cand) {
                Ok(()) => {
                    let _ = self.mt.remove(cand);
                    self.emit_eviction(cand, EvictionReason::Demoted);
                    return true;
                }
                Err(DmError::ActiveReferences(_)) => {}
                Err(_) => {}
            }
        }
        false
    }

    // mirrors src/lib.rs:1129 (evict_for_space)
    fn evict_for_space(&self, needed: u32, max_attempts: usize) -> Result<(), DErr> {
        const MAX_SCAN: usize = 4;
        let mut attempts = 0usize;
        while self.mt.used() + needed as usize > self.mt.capacity() {
            attempts += 1;
            if attempts > max_attempts {
                return Err(DErr::AllocationFailed);
            }
            let scan = (MAX_SCAN * attempts).min(1024);
            if !self.evict_one_clean(scan) {
                return Err(DErr::AllocationFailed);
            }
        }
        Ok(())
    }

    // mirrors src/lib.rs:1182 (evict_and_insert)
    fn evict_and_insert(&self, key: CacheKey, size: u32, max_attempts: usize) -> Result<usize, DErr> {
        const MAX_SCAN: usize = 4;
        let mut attempts = 0usize;
        loop {
            self.evict_for_space(size, max_attempts.saturating_sub(attempts))?;
            match self.mt.insert(key, size) {
                Ok(ptr) => return Ok(ptr),
                Err(MtError::PoolFull) => {
                    attempts += 1;
                    if attempts >= max_attempts {
                        return Err(DErr::AllocationFailed);
                    }
                    if !self.evict_one_clean(MAX_SCAN * attempts) {
                        return Err(DErr::AllocationFailed);
                    }
                }
                Err(MtError::AlreadyExists(k)) => return Err(DErr::AlreadyExists(k)),
                Err(_) => return Err(DErr::AllocationFailed),
            }
        }
    }

    // mirrors src/lib.rs:1268 (serve_memory_tier_to_gpu): the H2D copy reads the
    // slot; the value read is what the GPU receives.
    fn serve_memory_tier_to_gpu(&self, key: CacheKey, pointer: usize, _size: u32) -> u64 {
        if let Some(h) = &self.hook {
            h(Hook::ReadSection(key));
        }
        self.mt.read_slot(pointer)
    }

    // mirrors src/lib.rs:1355 (serve_concurrently_promoted); 50 µs sleep ->
    // yield_now, the 5 s timeout never fires. Returns the value delivered to
    // the caller's GPU buffer.
    fn serve_concurrently_promoted(&self, key: CacheKey) -> Result<u64, DErr> {
        loop {
            match self.dm.lookup(key) {
                Ok(LookupResult::MemoryTier { pointer, size }) => {
                    let v = self.serve_memory_tier_to_gpu(key, pointer, size);
                    let _ = self.dm.release_read(key);
                    let _ = self.dm.touch(key);
                    return Ok(v);
                }
                Ok(LookupResult::BlockDevice { .. }) => {
                    let _ = self.dm.release_read(key);
                    thread::yield_now();
                }
                Ok(LookupResult::NotExist) => return Err(DErr::KeyNotFound(key)),
                Err(_) => return Err(DErr::IoError),
            }
        }
    }

    // mirrors src/lib.rs:760 (promote_and_serve): reserve a slot, read the SSD
    // into it while streaming to the GPU, then flip the entry in the dispatch
    // map. The staging fallback (src/lib.rs:784-802) is not mirrored: no model
    // configures a staging pool, so tier saturation returns
    // AllocationFailed, as the real code does with no staging buffer. The
    // pipelined chunked read is the drive stand-in's `read`; its H2D to the
    // GPU is a slot read. Every failure exit after the slot was reserved
    // releases it (src/lib.rs:840-851, 910-918, 939-948): the dispatch map
    // still points the key at the SSD, so the slot would be dead capacity.
    // The real code guards the release with `!staged` (a staged serve holds a
    // pool buffer, not a tier slot); the mirror has no staging path at all,
    // so the unguarded remove is the same behaviour. Returns the value
    // delivered to the GPU.
    fn promote_and_serve(&self, key: CacheKey, offset: u64, size: u32) -> Result<u64, DErr> {
        let max_attempts = self.max_eviction_attempts;
        let has_drives = !self.drives.is_empty();
        // mirrors src/lib.rs:777 (evict_and_insert; reserves the slot)
        let mem_ptr = match self.evict_and_insert(key, size, max_attempts) {
            Ok(p) => p,
            Err(DErr::AllocationFailed) if has_drives => {
                // No staging pool configured (pipeline_ring.staging = None).
                return Err(DErr::AllocationFailed);
            }
            Err(e) => return Err(e),
        };

        if self.drives.is_empty() {
            // mirrors src/lib.rs:805-829 (no-hardware path): zeros to the GPU,
            // register the entry, release the write reference.
            let v = 0u64;
            if self.dm.create_memory_tier_entry(key, mem_ptr, size).is_ok() {
                // record_promotion_to_memory (counter dropped)
            }
            let _ = self.dm.release_write(key);
            return Ok(v);
        }

        let idx = drive_index(key, self.drives.len());
        let drive = &self.drives[idx];
        // mirrors src/lib.rs:840-851 (channel_pool.checkout()?; a failed
        // checkout releases the reserved slot)
        let lease = match drive.channel_pool.checkout() {
            Ok(l) => l,
            Err(_) => {
                let _ = self.mt.remove(key);
                return Err(DErr::IoError);
            }
        };
        // mirrors src/lib.rs:874-897 (pipelined_multi_object_zero_copy):
        // NVMe read into the slot, then the coalesced H2D to the GPU.
        let v = match drive.read(offset) {
            Ok(data) => {
                self.mt.write_slot(mem_ptr, data);
                self.mt.read_slot(mem_ptr)
            }
            // mirrors src/lib.rs:910-918: the read error is returned after the
            // reserved slot is released.
            Err(()) => {
                let _ = self.mt.remove(key);
                return Err(DErr::IoError);
            }
        };
        drop(lease);
        // staged (uncached) serve (src/lib.rs:927-930): not mirrored, see above.
        if self.m == Mutation::LookupColdSkipsPromote {
            // MUTANT(LookupColdSkipsPromote): serve without the promote transition.
            return Ok(v);
        }
        // mirrors src/lib.rs:939-948 (promote_block_to_memory_tier; a failed
        // transition releases the reserved slot)
        match self.dm.promote_block_to_memory_tier(key, mem_ptr, size) {
            Ok(()) => {}
            Err(_) => {
                let _ = self.mt.remove(key);
                return Err(DErr::IoError);
            }
        }
        Ok(v)
    }

    // mirrors src/lib.rs:1397 (process_write_job): the untracked path.
    pub fn process_write_job(
        dm: &DispatchMap,
        mt: &MemoryTier,
        drives: &[Arc<ExtentManager>],
        job: WriteJob,
    ) -> bool {
        let written = Self::write_through(mt, drives, &job);
        Self::settle_write_through(dm, job.key, written);
        written.is_some()
    }

    // mirrors src/lib.rs:1412 (run_write_job)
    pub fn run_write_job(
        pins: &WriteThroughPins,
        dm: &DispatchMap,
        mt: &MemoryTier,
        drives: &[Arc<ExtentManager>],
        job: WriteJob,
    ) {
        match pins.start(job.key) {
            JobStart::Stale => return,
            JobStart::Untracked => {
                let key = job.key;
                let landed = Self::process_write_job(dm, mt, drives, job);
                return pins.record_write(key, landed);
            }
            JobStart::Owned => {}
        }
        let written = Self::write_through(mt, drives, &job);
        pins.finish(job.key, |owned| {
            if owned {
                pins.record_write(job.key, written.is_some());
                Self::settle_write_through(dm, job.key, written);
                return;
            }
            let _ = mt.remove(job.key);
            if let (Some(offset), false) = (written, drives.is_empty()) {
                let drive_idx = job.device_index % drives.len();
                drives[drive_idx].remove_extent(offset);
            }
        });
    }

    // mirrors src/lib.rs:1448 (settle_write_through)
    fn settle_write_through(dm: &DispatchMap, key: CacheKey, written: Option<u64>) {
        let written = if dm.mutation() == Mutation::SettleSkipsConvert {
            // MUTANT(SettleSkipsConvert): the SSD copy is never recorded.
            None
        } else {
            written
        };
        match written {
            Some(block_offset) => {
                let _ = dm.convert_to_storage(key, block_offset);
            }
            None => {
                let _ = dm.release_read(key);
            }
        }
    }

    // mirrors src/lib.rs:1466 (write_through). The DmaBuffer wrap cannot fail in
    // the stand-in; reserve_extent fails when the drive is full and
    // write_buffer_to_ssd (src/lib.rs:672) fails when the drive injects an I/O
    // error, in which case the reservation is aborted (write_handle drop).
    fn write_through(mt: &MemoryTier, drives: &[Arc<ExtentManager>], job: &WriteJob) -> Option<u64> {
        let (mem_ptr, _size) = mt.peek(job.key)?;
        if drives.is_empty() {
            return Some(job.key * 4096);
        }
        let drive_idx = job.device_index % drives.len();
        let iem = &drives[drive_idx];
        let block_offset = iem.reserve_extent(job.key).ok()?;
        let data = mt.read_slot(mem_ptr);
        if iem.write(block_offset, data).is_err() {
            iem.abort(block_offset);
            return None;
        }
        iem.publish(block_offset);
        Some(block_offset)
    }

    // mirrors src/lib.rs:2257 (lookup) + src/lib.rs:3058 (lookup_async).
    // `size` is the caller's IpcHandle size. Returns the value delivered to
    // the caller's GPU buffer.
    pub fn lookup(&self, key: CacheKey, size: u32) -> Result<u64, DErr> {
        let result = self.dm.lookup(key);
        match result {
            Ok(LookupResult::NotExist) => Err(DErr::KeyNotFound(key)),
            Ok(LookupResult::MemoryTier { pointer, size }) => {
                let r = self.serve_memory_tier_to_gpu(key, pointer, size);
                let _ = self.dm.release_read(key);
                self.mt.touch(key);
                Ok(r)
            }
            Ok(LookupResult::BlockDevice { offset }) => {
                // mirrors src/lib.rs:3127-3157: the lookup's read pin is held
                // across the whole promotion; an AlreadyExists from
                // evict_and_insert means a concurrent promotion won the insert
                // race and is re-served warm.
                let r = match self.promote_and_serve(key, offset, size) {
                    Err(DErr::AlreadyExists(_)) => self.serve_concurrently_promoted(key),
                    r => r,
                };
                let _ = self.dm.release_read(key);
                r
            }
            Err(_) => Err(DErr::KeyNotFound(key)),
        }
    }

    // mirrors src/lib.rs:3242 (populate). `data` is the client GPU buffer.
    pub fn populate(&self, key: CacheKey, data: u64, size: u32) -> Result<(), DErr> {
        if size == 0 {
            return Err(DErr::InvalidParameter);
        }
        match self.reserve_memory(key, size, &|| false) {
            Ok(_) => {}
            Err(DErr::AllocationFailed) => return Ok(()),
            Err(e) => return Err(e),
        }
        if self.m == Mutation::PublishBeforeCopy {
            // MUTANT(PublishBeforeCopy): register + enqueue before the D2H lands.
            self.copy_gpu_to_memory_completed(key, size)?;
            self.copy_gpu_to_memory_async(key, data)?;
        } else {
            // Phase 2: every failure before Phase 3 registers the slot releases it.
            if let Err(e) = self.copy_gpu_to_memory_async(key, data) {
                let _ = self.release_memory(key);
                return Err(e);
            }
            self.copy_gpu_to_memory_completed(key, size)?;
        }
        Ok(())
    }

    // mirrors src/lib.rs:3434 (reserve_memory). `deadline_passed` stands for
    // `Instant::now() >= deadline`; sleep(POLL) -> yield_now.
    pub fn reserve_memory(
        &self,
        key: CacheKey,
        size: u32,
        deadline_passed: &dyn Fn() -> bool,
    ) -> Result<usize, DErr> {
        if size == 0 {
            return Err(DErr::InvalidParameter);
        }
        let max_attempts = self.max_eviction_attempts;
        if !self.store_backpressure {
            return self.evict_and_insert(key, size, max_attempts);
        }
        loop {
            match self.evict_and_insert(key, size, max_attempts) {
                Ok(mem_ptr) => return Ok(mem_ptr),
                Err(DErr::AllocationFailed) if !deadline_passed() => {
                    if self.m == Mutation::ReserveFailFast {
                        // MUTANT(ReserveFailFast): give up on the first refusal.
                        return Err(DErr::AllocationFailed);
                    }
                    thread::yield_now();
                }
                Err(e) => return Err(e),
            }
        }
    }

    // mirrors src/lib.rs:3538 (copy_gpu_to_memory_async), single region, synced.
    pub fn copy_gpu_to_memory_async(&self, key: CacheKey, data: u64) -> Result<(), DErr> {
        let (mem_ptr, _slot_size) = self.mt.get(key).ok_or(DErr::KeyNotFound(key))?;
        self.mt.write_slot(mem_ptr, data);
        Ok(())
    }

    // mirrors src/lib.rs:3706 (release_memory): idempotent slot free.
    pub fn release_memory(&self, key: CacheKey) -> Result<(), DErr> {
        match self.mt.remove(key) {
            Ok(()) | Err(MtError::KeyNotFound(_)) => Ok(()),
            Err(_) => Err(DErr::IoError),
        }
    }

    // mirrors src/lib.rs:3605 (copy_gpu_to_memory_completed)
    pub fn copy_gpu_to_memory_completed(&self, key: CacheKey, size: u32) -> Result<(), DErr> {
        let (mem_ptr, _) = self.mt.get(key).ok_or(DErr::KeyNotFound(key))?;
        self.dm.create_memory_tier_entry(key, mem_ptr, size).map_err(|e| match e {
            DmError::AlreadyExists(k) => {
                // Free the slot only when the existing entry provably lives elsewhere.
                let live = match self.dm.lookup(key) {
                    Ok(LookupResult::NotExist) => false,
                    Ok(r) => {
                        let _ = self.dm.release_read(key);
                        matches!(r, LookupResult::MemoryTier { pointer, .. } if pointer == mem_ptr)
                    }
                    Err(_) => true,
                };
                if !live {
                    let _ = self.mt.remove(key);
                }
                DErr::AlreadyExists(k)
            }
            _ => {
                let _ = self.mt.remove(key);
                DErr::IoError
            }
        })?;
        self.dm.downgrade_reference(key).map_err(|_| DErr::IoError)?;
        let num_drives = self.drives.len().max(1);
        let guard = self.bg_writer.lock().unwrap();
        // Record the pin before its job can run, so `remove` can take it over.
        self.write_through_pins.pin(key, || match *guard {
            Some(ref writer) => writer
                .enqueue(WriteJob { key, size, device_index: drive_index(key, num_drives) })
                .is_ok(),
            None => false,
        });
        Ok(())
    }

    // mirrors src/lib.rs:2325 (batch_lookup): classification (2476), cold
    // promotion in no-drives mode (2584) and the pooled cold path (2633),
    // warm pin release after the cold block (2879), concurrent-promotion
    // recovery (3066) and attribution (3081). The remote pass is not mirrored.
    pub fn batch_lookup(&self, entries: &[(CacheKey, u32)]) -> Vec<Outcome> {
        let n = entries.len();
        let mut results: Vec<Option<Result<(), DErr>>> = vec![None; n];
        let mut tier: Vec<Option<ServedBy>> = vec![None; n];
        let mut overwritten = vec![false; n];
        let mut tier_rewritten = vec![false; n];
        let mut deferred_touch_keys = Vec::new();
        let mut warm_pins: Vec<CacheKey> = Vec::new();
        // (idx, key, total_size, ssd offset)
        let mut cold_entries: Vec<(usize, CacheKey, u32, u64)> = Vec::new();

        for (i, &(key, total_size)) in entries.iter().enumerate() {
            const LOOKUP_RETRIES: u32 = 5;
            let lookup = {
                let mut lk = self.dm.lookup(key);
                let mut retries = 0u32;
                while lk.is_err() && retries < LOOKUP_RETRIES {
                    retries += 1;
                    lk = self.dm.lookup(key);
                }
                lk
            };
            match lookup {
                Ok(LookupResult::NotExist) => results[i] = Some(Err(DErr::KeyNotFound(key))),
                Ok(LookupResult::MemoryTier { pointer, size }) => {
                    let _ = self.serve_memory_tier_to_gpu(key, pointer, size);
                    warm_pins.push(key);
                    deferred_touch_keys.push(key);
                    tier[i] = Some(ServedBy::Dram);
                    results[i] = Some(Ok(()));
                }
                Ok(LookupResult::BlockDevice { offset }) => {
                    let _ = self.dm.release_read(key);
                    tier[i] = Some(ServedBy::Ssd);
                    cold_entries.push((i, key, total_size, offset));
                }
                Err(_) => results[i] = Some(Err(DErr::KeyNotFound(key))),
            }
        }
        if !deferred_touch_keys.is_empty() {
            self.mt.batch_touch(&deferred_touch_keys);
        }

        if !cold_entries.is_empty() {
            let max_attempts = self.max_eviction_attempts;
            if self.drives.is_empty() {
                // mirrors src/lib.rs:2584-2611 (no-hardware cold path)
                for &(idx, key, total_size, _offset) in &cold_entries {
                    self.evict_for_space(total_size, max_attempts).ok();
                    let res = self
                        .mt
                        .insert(key, total_size)
                        .map(|mem_ptr| {
                            if self.dm.promote_block_to_memory_tier(key, mem_ptr, total_size).is_ok() {
                                // record_promotion_to_memory (counter dropped)
                            } else {
                                // mirrors src/lib.rs:2592-2597: the promotion
                                // did not land — release the slot.
                                let _ = self.mt.remove(key);
                            }
                        })
                        .map_err(|e| match e {
                            MtError::AlreadyExists(k) => DErr::AlreadyExists(k),
                            _ => DErr::AllocationFailed,
                        });
                    results[idx] = Some(res);
                }
            } else {
                // mirrors src/lib.rs:2633-2845 (pooled cold path)
                self.batch_cold_pooled(&mut results, &cold_entries, max_attempts);
            }
        }

        // Warm stream sync, then PinnedKeys drop releases the warm pins.
        for key in warm_pins {
            let _ = self.dm.release_read(key);
        }

        for (i, &(key, _)) in entries.iter().enumerate() {
            if matches!(results[i], Some(Err(DErr::AlreadyExists(_)))) {
                let before = tier[i];
                results[i] = Some(self.serve_concurrently_promoted(key).map(|_| ()));
                if self.m == Mutation::RecoveryWrongTier {
                    // MUTANT(RecoveryWrongTier): rewrite the tier, to the wrong route.
                    tier[i] = Some(ServedBy::Remote);
                } else {
                    // Served out of DRAM after the winner's promotion.
                    tier[i] = Some(ServedBy::Dram);
                }
                overwritten[i] = true;
                tier_rewritten[i] = tier[i] != before;
            }
        }

        results
            .into_iter()
            .zip(tier)
            .enumerate()
            .map(|(i, (r, t))| {
                let result = r.unwrap();
                let served_by = match (&result, t) {
                    (Ok(()), Some(t)) => t,
                    (Ok(()), None) => ServedBy::Error,
                    (Err(DErr::KeyNotFound(_)), _) => ServedBy::Miss,
                    (Err(DErr::InvalidParameter), _) => ServedBy::SizeMismatch,
                    (Err(_), _) => ServedBy::Error,
                };
                Outcome {
                    served_by,
                    result,
                    overwritten: overwritten[i],
                    tier_rewritten: tier_rewritten[i],
                }
            })
            .collect()
    }

    // mirrors src/lib.rs:2633-2845 (the pooled cold path of batch_lookup):
    // per-entry evict_and_insert prep, GPU stream setup, per-drive channel
    // checkout, the multiplexed scatter read and the promote pass. Staging is
    // not mirrored (staging_available is always false: no model configures a
    // staging pool), so tier saturation is a prep failure. The H2D to the GPU
    // of multi-region entries is not mirrored (models are single-region).
    fn batch_cold_pooled(
        &self,
        results: &mut [Option<Result<(), DErr>>],
        cold: &[(usize, CacheKey, u32, u64)],
        max_attempts: usize,
    ) {
        let num_drives = self.drives.len();
        let mut drive_job_sets: Vec<DriveJobSet> = Vec::new();
        let mut prep_failures: Vec<(usize, Result<(), DErr>)> = Vec::new();

        // mirrors src/lib.rs:2642-2687 (prep: group by drive, reserve slots)
        for &(idx, key, size, offset) in cold {
            let drive_idx = drive_index(key, num_drives);
            // mirrors src/lib.rs:2617 (evict_and_insert reserves the slot)
            let prep = self.evict_and_insert(key, size, max_attempts);
            match prep {
                Ok(mem_ptr) => {
                    let job = DriveJob { idx, key, size, offset };
                    match drive_job_sets.iter_mut().find(|s| s.drive_idx == drive_idx) {
                        Some(set) => {
                            set.jobs.push(job);
                            set.mem_ptrs.push(mem_ptr);
                        }
                        None => drive_job_sets.push(DriveJobSet {
                            drive_idx,
                            jobs: vec![job],
                            mem_ptrs: vec![mem_ptr],
                        }),
                    }
                }
                // The staged arm (src/lib.rs:2636) cannot fire: no staging pool.
                Err(e) => prep_failures.push((idx, Err(e))),
            }
        }

        // mirrors src/lib.rs:2690-2692 (record prep failures)
        for (idx, res) in prep_failures {
            results[idx] = Some(res);
        }

        if drive_job_sets.is_empty() {
            return;
        }

        // mirrors src/lib.rs:2716-2745 (pipe stream setup): dev_streams is
        // None in this mirror, so the two streams come from gpu.create_stream;
        // the IGpuServices stand-in fails them on demand.
        if self.gpu_fail_create_stream {
            // mirrors src/lib.rs:2720-2734: fail every job; no I/O ran, so
            // each reserved slot is released.
            for djs in &drive_job_sets {
                for job in &djs.jobs {
                    let _ = self.mt.remove(job.key);
                    results[job.idx] = Some(Err(DErr::IoError));
                }
            }
            return; // drive_job_sets.clear()
        }

        // mirrors src/lib.rs:2746-2775 (one channel checkout per active drive;
        // the first failure releases every reserved slot — the scatter-gather
        // cannot run for any drive — and fails that drive's jobs)
        let mut channel_leases = Vec::new();
        let mut checkout_ok = true;
        for djs in &drive_job_sets {
            match self.drives[djs.drive_idx].channel_pool.checkout() {
                Ok(lease) => channel_leases.push(lease),
                Err(_) => {
                    for djs2 in &drive_job_sets {
                        for job in &djs2.jobs {
                            let _ = self.mt.remove(job.key);
                        }
                    }
                    for job in &djs.jobs {
                        results[job.idx] = Some(Err(DErr::IoError));
                    }
                    checkout_ok = false;
                    break;
                }
            }
        }

        if checkout_ok {
            // mirrors src/lib.rs:2785-2842 (scatter_gather_multi_drive_zero_copy,
            // then the per-job promote). One drive per model: the multiplexed
            // loop is the drive read of each job in turn. A failed read or a
            // failed promote releases the job's reserved slot
            // (src/lib.rs:2795-2838).
            for djs in &drive_job_sets {
                let drive = &self.drives[djs.drive_idx];
                for (ji, job) in djs.jobs.iter().enumerate() {
                    let res = match drive.read(job.offset) {
                        Ok(data) => {
                            self.mt.write_slot(djs.mem_ptrs[ji], data);
                            if self.m == Mutation::BatchColdSkipsPromote {
                                // MUTANT(BatchColdSkipsPromote): never promote.
                                Ok(())
                            } else {
                                // mirrors src/lib.rs:2798-2828
                                match self.dm.promote_block_to_memory_tier(
                                    job.key,
                                    djs.mem_ptrs[ji],
                                    job.size,
                                ) {
                                    Ok(()) => Ok(()),
                                    Err(_) => {
                                        // mirrors src/lib.rs:2818-2826: the
                                        // promotion did not land — release
                                        // the slot.
                                        let _ = self.mt.remove(job.key);
                                        Err(DErr::IoError)
                                    }
                                }
                            }
                        }
                        // mirrors src/lib.rs:2830-2833: Err(e) => Err(e.clone());
                        // the read failed — release the reserved slot.
                        Err(()) => {
                            let _ = self.mt.remove(job.key);
                            Err(DErr::IoError)
                        }
                    };
                    results[job.idx] = Some(res);
                }
            }
        }
        // channel_leases drop here, returning channels to the pools.
    }

    // mirrors src/lib.rs:3186 (remove)
    pub fn remove(&self, key: CacheKey) -> Result<(), DErr> {
        let block_offset = match self.dm.lookup(key) {
            Ok(LookupResult::BlockDevice { offset }) => {
                let _ = self.dm.release_read(key);
                Some(offset)
            }
            Ok(LookupResult::MemoryTier { .. }) => {
                let _ = self.dm.release_read(key);
                None
            }
            // `Ok(_)`: NotExist takes no read reference; the real code releases
            // anyway (a no-op KeyNotFound error).
            Ok(LookupResult::NotExist) => {
                let _ = self.dm.release_read(key);
                None
            }
            Err(_) => return Err(DErr::KeyNotFound(key)),
        };
        let job_frees_slot = match self.dm.remove(key) {
            Ok(()) => false,
            Err(DmError::ActiveReferences(_)) => self
                .write_through_pins
                .remove_entry(&self.dm, key)
                .ok_or(DErr::KeyNotFound(key))?,
            Err(_) => return Err(DErr::KeyNotFound(key)),
        };
        if !job_frees_slot && self.m != Mutation::RemoveSkipsSlotFree {
            // MUTANT(RemoveSkipsSlotFree): slot never freed.
            let _ = self.mt.remove(key);
        }
        if let Some(offset) = block_offset {
            let idx = drive_index(key, self.drives.len().max(1));
            if let Some(em) = self.drives.get(idx) {
                em.remove_extent(offset);
            }
        }
        Ok(())
    }

    // mirrors src/lib.rs:3929 (clear_memory_tier); 50 µs sleep -> yield_now,
    // the 5 s timeout never fires.
    pub fn clear_memory_tier(&self) -> usize {
        let mut count = 0;
        let mut pinned = 0;
        loop {
            let candidates = self.mt.oldest_keys(64 + pinned);
            if candidates.is_empty() {
                break;
            }
            let mut cleared = 0;
            pinned = 0;
            for key in candidates {
                if self.m == Mutation::ClearFreesFirst {
                    // MUTANT(ClearFreesFirst): slot freed before the demotion attempt.
                    let _ = self.mt.remove(key);
                }
                let done = match self.dm.try_evict_to_block(key) {
                    Ok(()) => true,
                    Err(DmError::ActiveReferences(_)) => false,
                    Err(_) => !matches!(self.dm.remove(key), Err(DmError::ActiveReferences(_))),
                };
                if done {
                    let _ = self.mt.remove(key);
                    count += 1;
                    cleared += 1;
                } else {
                    pinned += 1;
                }
            }
            if cleared == 0 {
                thread::yield_now();
            }
        }
        count
    }

    // mirrors src/lib.rs:3995 (flush_to_ssd); the bg_writer lock is held
    // across the flush, as in the real `if let Some(ref writer) = *lock()`.
    // `oldest_keys(usize::MAX)` is every resident key.
    pub fn flush_to_ssd(&self) -> Result<usize, DErr> {
        if let Some(ref writer) = *self.bg_writer.lock().unwrap() {
            if self.m != Mutation::FlushNoWait {
                // MUTANT(FlushNoWait): return without waiting for the queue.
                writer.flush();
            }
        }
        let failed = self.write_through_pins.failed_writes(|key| self.mt.contains(key));
        if failed > 0 {
            return Err(DErr::IoError);
        }
        Ok(self.mt.oldest_keys(usize::MAX).into_iter().filter(|&key| self.dm.is_evictable(key)).count())
    }

    // mirrors src/lib.rs:3836 (promote_to_memory_tier): classify the keys,
    // then promote every block-device entry into the memory tier. Reporting
    // (promotions-to-memory counter) is dropped. Takes the dispatcher by Arc:
    // each drive's pass runs on its own thread (loom spawn has no scopes).
    pub fn promote_to_memory_tier(d: Arc<Self>, keys: &[CacheKey]) {
        // (key, offset, size)
        let mut cold_entries: Vec<(CacheKey, u64, u32)> = Vec::new();

        // mirrors src/lib.rs:3857-3872 (classification)
        for &key in keys {
            match d.dm.lookup(key) {
                Ok(LookupResult::BlockDevice { offset }) => {
                    let _ = d.dm.release_read(key);
                    if let Ok(size) = d.dm.entry_size(key) {
                        cold_entries.push((key, offset, size));
                    }
                }
                Ok(LookupResult::MemoryTier { .. }) => {
                    let _ = d.dm.release_read(key);
                    let _ = d.dm.touch(key);
                    d.mt.touch(key);
                }
                _ => {}
            }
        }

        if cold_entries.is_empty() {
            return;
        }

        if d.drives.is_empty() {
            // mirrors src/lib.rs:3881-3901 (no-hardware path)
            let max_attempts = d.max_eviction_attempts;
            for &(key, _offset, size) in &cold_entries {
                if d.evict_for_space(size, max_attempts).is_err() {
                    continue;
                }
                if let Ok(mem_ptr) = d.mt.insert(key, size) {
                    // In-place promote (pin-safe): no remove/recreate.
                    if d.dm.promote_block_to_memory_tier(key, mem_ptr, size).is_ok() {
                        // record_promotion_to_memory
                    } else {
                        // mirrors src/lib.rs:3894-3898: the promotion did not
                        // land — release the slot.
                        let _ = d.mt.remove(key);
                    }
                }
            }
            return;
        }

        // mirrors src/lib.rs:3908-3991: group by target drive and run each
        // drive's pass on its own thread, joined before returning (the real
        // code uses std::thread::scope; loom has spawn+join).
        let num_drives = d.drives.len();
        let mut per_drive: Vec<Vec<(CacheKey, u64, u32)>> = vec![Vec::new(); num_drives];
        for entry in &cold_entries {
            per_drive[drive_index(entry.0, num_drives)].push(*entry);
        }
        let mut handles = Vec::new();
        for (drive_idx, entry_indices) in per_drive.into_iter().enumerate() {
            if entry_indices.is_empty() {
                continue;
            }
            let dd = d.clone();
            handles.push(thread::spawn(move || dd.promote_drive_pass(drive_idx, entry_indices)));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    // mirrors src/lib.rs:3935-3991 (one drive's body inside the scope).
    fn promote_drive_pass(&self, drive_idx: usize, cold: Vec<(CacheKey, u64, u32)>) {
        let max_attempts = self.max_eviction_attempts;
        let drive = &self.drives[drive_idx];
        // mirrors src/lib.rs:3939-3942 (channel checkout before any insert;
        // on failure the thread returns without promoting anything)
        let _lease = match drive.channel_pool.checkout() {
            Ok(l) => l,
            Err(_) => return,
        };
        for &(key, offset, size) in &cold {
            // mirrors src/lib.rs:3949-3954
            if self.evict_for_space(size, max_attempts).is_err() {
                continue;
            }
            // mirrors src/lib.rs:3956-3959
            let mem_ptr = match self.mt.insert(key, size) {
                Ok(ptr) => ptr,
                Err(_) => continue,
            };
            // mirrors src/lib.rs:3962-3972 (pipelined_ssd_to_dram_only)
            match drive.read(offset) {
                Err(()) => {
                    if self.m != Mutation::PromoteSkipsReadFailRelease {
                        // MUTANT(PromoteSkipsReadFailRelease): keep the slot
                        // after the failed read.
                        let _ = self.mt.remove(key); // src/lib.rs:3975
                    }
                    // debug log dropped
                }
                Ok(data) => {
                    self.mt.write_slot(mem_ptr, data);
                    // mirrors src/lib.rs:3982-3996 (in-place, pin-safe promote;
                    // a failed transition releases the slot, as the read-failure
                    // exit above does)
                    if self.dm.promote_block_to_memory_tier(key, mem_ptr, size).is_ok() {
                        // record_promotion_to_memory
                    } else {
                        let _ = self.mt.remove(key);
                    }
                }
            }
        }
    }
}
