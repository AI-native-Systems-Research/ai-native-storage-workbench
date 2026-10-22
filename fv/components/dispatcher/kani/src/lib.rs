//! Kani harnesses for `components/dispatcher` (pin 7d280cd3).
//!
//! * [`model`] — concrete models of the bound collaborators (dispatch map, memory
//!   tier, GPU services, remote lookup, logger, extent manager).
//! * [`mirror`] — the DISCLOSED MIRROR: the dispatcher method bodies the properties
//!   are about, transcribed with line references (see its module docs for why the
//!   real `DispatcherComponent` could not be driven under Kani, and what is elided).
//!   The real `dispatcher::TierEventCounters`, `dispatcher::io_segmenter::segment_io`
//!   and `interfaces::ReadWriteStats::merge_from` are called directly.
//! * `proofs` — one `refute_<id>` harness per work-list property.

pub mod mirror;
pub mod model;

#[cfg(kani)]
mod proofs;
