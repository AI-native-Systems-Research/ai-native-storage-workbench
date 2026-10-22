/*
 * CONTROL copy of flush_clear.pml: the same model with the minimal code fix switched on (-DFIX).
 * No property, assert, actor or bound is changed; only flush_to_ssd and the writer's failure branch differ.
 *
 * The fix, in Rust terms:
 *   - background.rs: the worker counts failed write-through jobs (process_write_job reports Err on the
 *     DmaBuffer / reserve_extent / SSD-write exits, lib.rs:1436-1461, instead of returning ()).
 *   - flush_to_ssd (lib.rs:3894-3907): after writer.flush(), if any job since the last flush failed,
 *     return Err(DispatcherError::IoError("write-through failed for N entries")); otherwise return
 *     Ok(number of entries that now have a valid SSD offset), as the interface doc says.
 *   - a caller that gets Err does not call clear_memory_tier (modelled; clear itself is unchanged).
 */
#define FIX 1
#include "flush_clear.pml"
