//! Collaborator models used by the mirror (`mirror.rs`).
//!
//! Concrete, single-threaded models (no `dyn`) of the components the dispatcher
//! binds through receptacles. Each models the DOCUMENTED semantics of the real
//! component, using the real `interfaces` result/error types:
//!
//! * [`Dm`] — `components/dispatch-map/src/lib.rs`: `lookup` takes a read ref on a
//!   present entry and returns `Err(Timeout)` when a writer holds `write_ref` past the
//!   wait (lib.rs:118-160); `remove` refuses a pinned entry with `ActiveReferences`
//!   (lib.rs:324-335); `create_memory_tier_entry` refuses an existing key with
//!   `AlreadyExists` (lib.rs:381-395); `promote_block_to_memory_tier` flips a
//!   `BlockDevice` entry in place keeping its offset (lib.rs:467-500);
//!   `try_evict_to_block` is `KeyNotFound` for an absent key, `ActiveReferences` on a
//!   pin and `InvalidState` without an SSD offset (lib.rs:533-563). At most 2 entries.
//! * [`Mt`] — memory tier: `insert` hands out a slot or `AlreadyExists` when the key
//!   is resident; `remove` frees it; `used`/`capacity` in bytes.
//! * [`Gpu`] — GPU services: every call is recorded; `stream_synchronize` and
//!   `register_host_memory` can be made to fail (CUDA errors the real service returns
//!   as `Err`).
//!
//! Every injected fault is a behaviour the real collaborator exhibits; each harness
//! says which (a writer holding `write_ref`, a sibling lookup winning the
//! `mt.insert` race, a CUDA error, a full extent manager).

use std::cell::{Cell, UnsafeCell};

use interfaces::{
    CacheKey, DispatchMapError, GpuStream, LookupResult, MemoryTierError, RemoteLookupError,
};

/// Single-threaded interior mutability without `RefCell`'s borrow flag (whose
/// branches CBMC otherwise carries through every merge). No caller ever holds two
/// overlapping borrows of one cell: each model method takes one borrow and releases
/// it before returning.
pub struct RefCell<T>(UnsafeCell<T>);

impl<T: Default> Default for RefCell<T> {
    fn default() -> Self {
        RefCell::new(T::default())
    }
}

impl<T> RefCell<T> {
    pub const fn new(v: T) -> Self {
        RefCell(UnsafeCell::new(v))
    }
    pub fn borrow(&self) -> &T {
        // SAFETY: single-threaded; no overlapping mutable borrow (see above).
        unsafe { &*self.0.get() }
    }
    #[allow(clippy::mut_from_ref)]
    pub fn borrow_mut(&self) -> &mut T {
        // SAFETY: single-threaded; no overlapping borrow (see above).
        unsafe { &mut *self.0.get() }
    }
}

/// Event clock shared by all models, so a harness can compare call order across
/// collaborators.
pub struct Clock(Cell<u32>);

impl Clock {
    pub const fn new() -> Self {
        Clock(Cell::new(1))
    }
    pub fn tick(&self) -> u32 {
        let t = self.0.get();
        self.0.set(t + 1);
        t
    }
}

// ---------------------------------------------------------------------------
// Dispatch map
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    Block { offset: u64 },
    Mem { pointer: *mut u8, size: u32, ssd_offset: Option<u64> },
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub key: CacheKey,
    pub loc: Loc,
    pub read_ref: u32,
    pub write_ref: u32,
}

impl Entry {
    pub fn block(key: CacheKey, offset: u64) -> Self {
        Entry { key, loc: Loc::Block { offset }, read_ref: 0, write_ref: 0 }
    }
    pub fn mem(key: CacheKey, pointer: *mut u8, size: u32, ssd_offset: Option<u64>) -> Self {
        Entry { key, loc: Loc::Mem { pointer, size, ssd_offset }, read_ref: 0, write_ref: 0 }
    }
}

/// A concurrent promoter (a sibling `batch_lookup`, e.g. the other tensor-parallel
/// rank) that finishes its SSD->DRAM read and flips `key` to `MemoryTier` right after
/// our `after`-th lookup of it.
#[derive(Clone, Copy, Debug)]
pub struct Flip {
    pub key: CacheKey,
    pub after: u32,
    pub pointer: *mut u8,
    pub size: u32,
}

#[derive(Default)]
pub struct DmState {
    pub entries: [Option<Entry>; 2],
    /// Upcoming `lookup`s that time out because a writer holds `write_ref`.
    pub lookup_timeouts: u32,
    pub flip: Option<Flip>,
    pub lookups: u32,
    pub removes_ok: u32,
    pub removes_refused: u32,
    pub t_remove_ok: u32,
    pub promotes_ok: u32,
    pub creates_refused: u32,
    pub evicted_to_block: u32,
}

pub struct Dm<'c> {
    pub s: RefCell<DmState>,
    pub clock: &'c Clock,
}

fn slot_idx(entries: &[Option<Entry>; 2], key: CacheKey) -> Option<usize> {
    if matches!(entries[0], Some(e) if e.key == key) {
        Some(0)
    } else if matches!(entries[1], Some(e) if e.key == key) {
        Some(1)
    } else {
        None
    }
}

fn slot(s: &mut DmState, key: CacheKey) -> Option<&mut Entry> {
    match slot_idx(&s.entries, key) {
        Some(i) => s.entries[i].as_mut(),
        None => None,
    }
}

impl<'c> Dm<'c> {
    pub fn new(clock: &'c Clock, entries: [Option<Entry>; 2]) -> Self {
        Dm { s: RefCell::new(DmState { entries, ..Default::default() }), clock }
    }

    pub fn get(&self, key: CacheKey) -> Option<Entry> {
        let s = self.s.borrow();
        slot_idx(&s.entries, key).and_then(|i| s.entries[i])
    }

    pub fn lookup(&self, key: CacheKey) -> Result<LookupResult, DispatchMapError> {
        let s = self.s.borrow_mut();
        s.lookups += 1;
        if s.lookup_timeouts > 0 {
            s.lookup_timeouts -= 1;
            return Err(DispatchMapError::Timeout(key));
        }
        let flip = s.flip;
        let mut flipped = false;
        let e = match slot(s, key) {
            None => return Ok(LookupResult::NotExist),
            Some(e) => e,
        };
        e.read_ref += 1;
        let r = match e.loc {
            Loc::Block { offset } => LookupResult::BlockDevice { offset },
            Loc::Mem { pointer, size, .. } => {
                LookupResult::MemoryTier { pointer, size }
            }
        };
        if let Some(f) = flip {
            if f.key == key && f.after <= 1 {
                if let Loc::Block { offset } = e.loc {
                    e.loc = Loc::Mem { pointer: f.pointer, size: f.size, ssd_offset: Some(offset) };
                }
                flipped = true;
            }
        }
        if let Some(f) = flip {
            if f.key == key {
                s.flip = if flipped { None } else { Some(Flip { after: f.after - 1, ..f }) };
            }
        }
        Ok(r)
    }

    pub fn release_read(&self, key: CacheKey) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        let e = slot(s, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        if e.read_ref == 0 {
            return Err(DispatchMapError::RefCountUnderflow(key));
        }
        e.read_ref -= 1;
        Ok(())
    }

    pub fn release_write(&self, key: CacheKey) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        let e = slot(s, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        if e.write_ref == 0 {
            return Err(DispatchMapError::NoWriteReference(key));
        }
        e.write_ref = 0;
        Ok(())
    }

    pub fn touch(&self, key: CacheKey) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        slot(s, key).map(|_| ()).ok_or(DispatchMapError::KeyNotFound(key))
    }

    pub fn remove(&self, key: CacheKey) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        let idx = slot_idx(&s.entries, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        let e = s.entries[idx].unwrap();
        if e.read_ref > 0 || e.write_ref > 0 {
            s.removes_refused += 1;
            return Err(DispatchMapError::ActiveReferences(key));
        }
        s.entries[idx] = None;
        s.removes_ok += 1;
        s.t_remove_ok = self.clock.tick();
        Ok(())
    }

    pub fn convert_to_storage(&self, key: CacheKey, offset: u64) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        let e = slot(s, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        if let Loc::Mem { ssd_offset, .. } = &mut e.loc {
            *ssd_offset = Some(offset);
        }
        e.read_ref = e.read_ref.saturating_sub(1);
        Ok(())
    }

    pub fn create_memory_tier_entry(
        &self,
        key: CacheKey,
        pointer: *mut u8,
        size: u32,
    ) -> Result<(), DispatchMapError> {
        if size == 0 {
            return Err(DispatchMapError::InvalidSize);
        }
        let s = self.s.borrow_mut();
        if slot(s, key).is_some() {
            s.creates_refused += 1;
            return Err(DispatchMapError::AlreadyExists(key));
        }
        let free = if s.entries[0].is_none() { 0 } else { 1 };
        assert!(s.entries[free].is_none(), "model map full");
        s.entries[free] = Some(Entry {
            key,
            loc: Loc::Mem { pointer, size, ssd_offset: None },
            read_ref: 0,
            write_ref: 1,
        });
        Ok(())
    }

    pub fn promote_block_to_memory_tier(
        &self,
        key: CacheKey,
        pointer: *mut u8,
        size: u32,
    ) -> Result<(), DispatchMapError> {
        if size == 0 {
            return Err(DispatchMapError::InvalidSize);
        }
        let s = self.s.borrow_mut();
        let e = slot(s, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        match e.loc {
            Loc::Block { offset } => {
                e.loc = Loc::Mem { pointer, size, ssd_offset: Some(offset) };
                s.promotes_ok += 1;
                Ok(())
            }
            Loc::Mem { .. } => Err(DispatchMapError::InvalidState(String::new())),
        }
    }

    pub fn try_evict_to_block(&self, key: CacheKey) -> Result<(), DispatchMapError> {
        let s = self.s.borrow_mut();
        let e = slot(s, key).ok_or(DispatchMapError::KeyNotFound(key))?;
        if e.read_ref != 0 || e.write_ref != 0 {
            return Err(DispatchMapError::ActiveReferences(key));
        }
        match e.loc {
            Loc::Mem { ssd_offset: Some(offset), .. } => {
                e.loc = Loc::Block { offset };
                s.evicted_to_block += 1;
                Ok(())
            }
            // no SSD offset (write-through not complete) / not in memory-tier state
            _ => Err(DispatchMapError::InvalidState(String::new())),
        }
    }
}

// ---------------------------------------------------------------------------
// Memory tier
// ---------------------------------------------------------------------------

pub struct MtState {
    pub slots: [Option<(CacheKey, u32)>; 2],
    pub capacity: usize,
    pub used: usize,
    /// A key a sibling lookup already inserted (it won the `mt.insert` race).
    pub resident_elsewhere: Option<CacheKey>,
    pub pool: *mut u8,
    pub pool_len: usize,
    pub removes_ok: u32,
    pub t_remove: u32,
}

fn mt_idx(slots: &[Option<(CacheKey, u32)>; 2], key: CacheKey) -> Option<usize> {
    if matches!(slots[0], Some(x) if x.0 == key) {
        Some(0)
    } else if matches!(slots[1], Some(x) if x.0 == key) {
        Some(1)
    } else {
        None
    }
}

pub struct Mt<'c> {
    pub s: RefCell<MtState>,
    pub clock: &'c Clock,
}

impl<'c> Mt<'c> {
    pub fn new(clock: &'c Clock, capacity: usize) -> Self {
        let pool: &'static mut [u8] = Box::leak(Box::new([0u8; 16]));
        Mt {
            s: RefCell::new(MtState {
                capacity,
                slots: [None, None],
                used: 0,
                resident_elsewhere: None,
                pool: pool.as_mut_ptr(),
                pool_len: pool.len(),
                removes_ok: 0,
                t_remove: 0,
            }),
            clock,
        }
    }

    /// Pre-load a resident slot.
    pub fn with_slot(self, key: CacheKey, size: u32) -> Self {
        {
            let s = self.s.borrow_mut();
            let free = if s.slots[0].is_none() { 0 } else { 1 };
            assert!(s.slots[free].is_none());
            s.slots[free] = Some((key, size));
            s.used += size as usize;
        }
        self
    }

    pub fn pool_ptr(&self) -> *mut u8 {
        self.s.borrow().pool
    }

    pub fn insert(&self, key: CacheKey, size: u32) -> Result<*mut u8, MemoryTierError> {
        let s = self.s.borrow_mut();
        if s.resident_elsewhere == Some(key) || mt_idx(&s.slots, key).is_some() {
            return Err(MemoryTierError::AlreadyExists(key));
        }
        if s.used + size as usize > s.capacity {
            return Err(MemoryTierError::PoolFull);
        }
        let free = if s.slots[0].is_none() {
            0
        } else if s.slots[1].is_none() {
            1
        } else {
            return Err(MemoryTierError::PoolFull);
        };
        s.slots[free] = Some((key, size));
        s.used += size as usize;
        Ok(s.pool)
    }

    pub fn peek(&self, key: CacheKey) -> Option<(*mut u8, u32)> {
        let s = self.s.borrow();
        mt_idx(&s.slots, key).map(|i| (s.pool, s.slots[i].unwrap().1))
    }

    pub fn remove(&self, key: CacheKey) -> Result<(), MemoryTierError> {
        let s = self.s.borrow_mut();
        let idx = mt_idx(&s.slots, key).ok_or(MemoryTierError::KeyNotFound(key))?;
        let size = s.slots[idx].unwrap().1;
        s.slots[idx] = None;
        s.used -= size as usize;
        s.removes_ok += 1;
        s.t_remove = self.clock.tick();
        Ok(())
    }

    /// Eviction order = slot order.
    pub fn oldest_keys(&self, n: usize) -> Vec<CacheKey> {
        let s = self.s.borrow();
        let mut v = Vec::new();
        if let Some(x) = s.slots[0] {
            if v.len() < n {
                v.push(x.0);
            }
        }
        if let Some(x) = s.slots[1] {
            if v.len() < n {
                v.push(x.0);
            }
        }
        v
    }

    pub fn touch(&self, _key: CacheKey) {}
    pub fn batch_touch(&self, _keys: &[CacheKey]) {}

    pub fn used(&self) -> usize {
        self.s.borrow().used
    }
    pub fn capacity(&self) -> usize {
        self.s.borrow().capacity
    }
    pub fn pool_info(&self) -> Option<(*mut u8, usize)> {
        let s = self.s.borrow();
        Some((s.pool, s.pool_len))
    }
}

// ---------------------------------------------------------------------------
// GPU services
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct GpuState {
    /// `device_of_ptr` answer: `>= 0` a known device, `-1` unknown.
    pub device: i32,
    pub fail_sync: bool,
    pub fail_register: bool,
    pub streams_created: u32,
    pub h2d_async: u32,
    pub last_h2d_stream: usize,
    pub dma_to_device: u32,
    pub syncs: u32,
    pub syncs_on: [u32; 2],
    pub registers: u32,
}

pub struct Gpu {
    pub s: RefCell<GpuState>,
}

/// Stream handles the model hands out: distinct, non-null, never dereferenced.
pub const STREAM_BASE: usize = 0x1000;

impl Gpu {
    pub fn new(device: i32) -> Self {
        Gpu { s: RefCell::new(GpuState { device, ..Default::default() }) }
    }
    pub fn device_of_ptr(&self, _p: *const std::ffi::c_void) -> Result<i32, String> {
        Ok(self.s.borrow().device)
    }
    pub fn set_device(&self, _d: i32) -> Result<(), String> {
        Ok(())
    }
    pub fn create_stream(&self) -> Result<GpuStream, String> {
        let s = self.s.borrow_mut();
        s.streams_created += 1;
        Ok(GpuStream((STREAM_BASE + s.streams_created as usize) as *mut std::ffi::c_void))
    }
    pub fn memcpy_h2d_async(
        &self,
        _src: *const std::ffi::c_void,
        _dst: *mut std::ffi::c_void,
        _size: usize,
        st: GpuStream,
    ) -> Result<(), String> {
        let s = self.s.borrow_mut();
        s.h2d_async += 1;
        s.last_h2d_stream = st.0 as usize;
        Ok(())
    }
    pub fn dma_copy_to_device(&self, _dst: *mut std::ffi::c_void, _size: usize) -> Result<(), String> {
        self.s.borrow_mut().dma_to_device += 1;
        Ok(())
    }
    pub fn stream_synchronize(&self, st: GpuStream) -> Result<(), String> {
        let s = self.s.borrow_mut();
        s.syncs += 1;
        let which = st.0 as usize;
        if which == STREAM_BASE + 1 {
            s.syncs_on[0] += 1;
        } else {
            s.syncs_on[1] += 1;
        }
        if s.fail_sync {
            Err(String::new())
        } else {
            Ok(())
        }
    }
    pub fn register_host_memory(&self, _p: *mut std::ffi::c_void, _n: usize) -> Result<(), String> {
        let s = self.s.borrow_mut();
        s.registers += 1;
        if s.fail_register {
            Err(String::new())
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Remote lookup, logger, extent manager
// ---------------------------------------------------------------------------

/// A cluster in which no peer holds any key (every forwarded key is NotFound).
#[derive(Default)]
pub struct Remote {
    pub forwarded: Cell<u32>,
}

impl Remote {
    pub fn batch_lookup(&self, entries: &[(CacheKey, u32)]) -> Vec<Result<(), RemoteLookupError>> {
        entries
            .iter()
            .map(|_| {
                self.forwarded.set(self.forwarded.get() + 1);
                Err(RemoteLookupError::NotFound)
            })
            .collect()
    }
}

#[derive(Default)]
pub struct Logger {
    pub warns: Cell<u32>,
    pub infos: Cell<u32>,
}

impl Logger {
    pub fn info(&self) {
        self.infos.set(self.infos.get() + 1);
    }
    pub fn warn(&self) {
        self.warns.set(self.warns.get() + 1);
    }
}

/// One data drive's extent manager: the set of allocated extent offsets.
#[derive(Default)]
pub struct Extents {
    pub allocated: RefCell<[Option<u64>; 2]>,
    /// `reserve_extent` fails (drive full).
    pub full: Cell<bool>,
}

impl Extents {
    pub fn with(offset: u64) -> Self {
        let e = Extents::default();
        e.allocated.borrow_mut()[0] = Some(offset);
        e
    }
    pub fn is_allocated(&self, offset: u64) -> bool {
        self.allocated.borrow().iter().any(|o| *o == Some(offset))
    }
    pub fn remove_extent(&self, offset: u64) -> Result<(), ()> {
        let a = self.allocated.borrow_mut();
        match a.iter().position(|o| *o == Some(offset)) {
            Some(i) => {
                a[i] = None;
                Ok(())
            }
            None => Err(()),
        }
    }
    pub fn reserve_extent(&self) -> Result<u64, ()> {
        if self.full.get() {
            Err(())
        } else {
            Ok(0)
        }
    }
}

/// The bounded eviction-event channel (`crossbeam_channel::bounded(capacity)`,
/// lib.rs:558-565). Only `try_send` is used by the dispatcher: it queues the event
/// if there is room and returns `Err` (Full) otherwise, never blocking.
pub struct EventChan {
    pub capacity: usize,
    pub queued: RefCell<Vec<interfaces::EvictionEvent>>,
}

impl EventChan {
    pub fn bounded(capacity: usize) -> Self {
        EventChan { capacity, queued: RefCell::new(Vec::new()) }
    }
    pub fn try_send(&self, ev: interfaces::EvictionEvent) -> Result<(), interfaces::EvictionEvent> {
        let q = self.queued.borrow_mut();
        if q.len() < self.capacity {
            q.push(ev);
            Ok(())
        } else {
            Err(ev)
        }
    }
    pub fn len(&self) -> usize {
        self.queued.borrow().len()
    }
}
