//! File-system events merged into batched row deltas.
//!
//! This is the `fam-sched` component of `docs/legacy-specs.md` §4: watcher output feeds a bounded
//! queue that drops when full, one consumer serialises it, and changes are merged per path before
//! they reach a sink.

mod batch;
mod pipeline;

pub use batch::{merge, Batch, Batcher, Change, FileChange};
pub use pipeline::{
    spawn, Handle, SchedError, SchedulerConfig, Sink, DEFAULT_MAX_BATCH, DEFAULT_QUEUE_CAPACITY,
};
