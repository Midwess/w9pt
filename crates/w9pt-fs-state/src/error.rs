//! Typed infrastructure failures kept separate from semantic store outcomes.

/// Store operation during which an adapter infrastructure failure occurred.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StateStoreOperation {
    /// Consistent record read.
    Read,
    /// Atomic declarative commit.
    Commit,
    /// Writer-lease acquisition.
    AcquireLease,
    /// Writer-lease renewal.
    RenewLease,
    /// Writer-lease release.
    ReleaseLease,
    /// Revision change polling.
    PollChanges,
}

/// Stable adapter/infrastructure failure category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AdapterFailureKind {
    /// Authority cannot currently be reached.
    Unavailable,
    /// Operation exceeded an adapter-owned deadline.
    Timeout,
    /// Native transaction serialization exhausted bounded retries.
    Serialization,
    /// Persisted adapter representation is corrupt or cannot round-trip.
    Corruption,
    /// Deployment cannot implement the requested contract operation.
    Unsupported,
    /// Adapter failed for another non-semantic reason.
    Internal,
}

/// Common behavior required from an adapter's associated infrastructure error.
pub trait StateStoreAdapterError: std::error::Error + Send + Sync + 'static {
    /// Returns the stable failure category.
    fn kind(&self) -> AdapterFailureKind;

    /// Returns the operation that failed.
    fn operation(&self) -> StateStoreOperation;
}
