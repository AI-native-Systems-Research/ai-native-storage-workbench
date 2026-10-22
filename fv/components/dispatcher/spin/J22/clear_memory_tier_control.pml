/*
 * CONTROL copy of clear_memory_tier.pml: the same model with the minimal code fix switched on.
 * No property, assert, actor or bound is changed; only the body of clear_memory_tier differs (#ifdef FIX).
 *
 * The fix, in Rust terms (components/dispatcher/src/lib.rs:3814-3821):
 *     match dm.try_evict_to_block(key) {
 *         Ok(())                                   => { let _ = mt.remove(key); count += 1; }
 *         Err(DispatchMapError::ActiveReferences(_)) => {}   // pinned: retry on the next pass
 *         Err(DispatchMapError::KeyNotFound(_))    => { let _ = mt.remove(key); count += 1; }  // orphan slot
 *         Err(_) /- InvalidState: unpersisted -/  => {
 *             if dm.remove(key).is_ok() { let _ = mt.remove(key); count += 1; }   // map entry first
 *         }
 *     }
 * The outer `loop` (lib.rs:3808) then retries a pinned key until its pin is released.
 */
#define FIX 1
#include "clear_memory_tier.pml"
