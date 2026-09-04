//! Deterministic reference authority and reusable adapter conformance support.

mod conformance;
mod memory;
#[cfg(test)]
mod scenarios;

pub use conformance::{
    StateStoreConformanceError, StateStoreConformanceHarness, check_state_store_conformance,
};
pub use memory::{
    CommitFailureTiming, MemoryAuthority, MemoryStateStore, MemoryStateStoreError,
    MemoryTraceEvent, MemoryTracePhase,
};
