//! Runtime-neutral target-object contract.
//!
//! A writable adapter must acknowledge an immutable put only after the exact bytes
//! are durable, provide atomic create-if-absent and compare-and-swap operations,
//! and make successful writes visible to subsequent reads. Ambiguous mutation
//! results are represented explicitly so their owning layer can resolve them by
//! reading the key back.

use core::future::Future;

use crate::{ConfigurationError, ObjectKey, ObjectVersion, RangeError};

/// Exact stored object bytes and their opaque target revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetObject {
    bytes: Vec<u8>,
    version: ObjectVersion,
}

impl TargetObject {
    /// Creates an exact target response.
    pub fn new(bytes: Vec<u8>, version: ObjectVersion) -> Self {
        Self { bytes, version }
    }

    /// Returns the complete object bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the response and returns the complete bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the opaque target revision.
    pub fn version(&self) -> &ObjectVersion {
        &self.version
    }
}

/// Checked half-open byte range for a target object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRange {
    start: u64,
    end: u64,
}

impl ObjectRange {
    /// Creates `[start, end)`, rejecting reversed bounds.
    pub const fn new(start: u64, end: u64) -> Result<Self, RangeError> {
        if end < start {
            Err(RangeError::InvalidOrder { start, end })
        } else {
            Ok(Self { start, end })
        }
    }

    /// Returns the inclusive start offset.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the exclusive end offset.
    pub const fn end(self) -> u64 {
        self.end
    }

    /// Returns the requested byte count.
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Reports whether the range contains no bytes.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Outcome of atomic immutable creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PutIfAbsent {
    /// The supplied bytes were durably created.
    Created {
        /// Revision assigned to the new object.
        version: ObjectVersion,
    },
    /// The key already existed and was not modified.
    AlreadyExists {
        /// Current revision of the existing object.
        version: ObjectVersion,
    },
    /// The adapter cannot determine whether the immutable creation committed.
    ///
    /// The caller must not assume the key is absent or publish any dependent
    /// object until bounded exact readback proves the desired bytes are present.
    Ambiguous,
}

/// Outcome of atomic single-key compare-and-swap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompareExchange {
    /// The supplied bytes durably replaced or created the key.
    Replaced {
        /// Revision assigned to the published object.
        version: ObjectVersion,
    },
    /// The expected revision did not match and the key was not modified.
    Conflict {
        /// Current revision, or `None` when the key is absent.
        current: Option<ObjectVersion>,
    },
    /// The adapter cannot determine whether the replacement committed.
    Ambiguous,
}

/// Semantic guarantees advertised by a target adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetGuarantees {
    /// Successful put/CAS acknowledgments mean bytes are durable.
    pub durable_writes: bool,
    /// Immutable creation is atomic for one key.
    pub atomic_put_if_absent: bool,
    /// Compare-and-swap is atomic for one key and opaque revision.
    pub atomic_compare_exchange: bool,
    /// Successful publication is immediately visible to exact reads.
    pub read_after_write: bool,
}

impl TargetGuarantees {
    /// No writable semantics have been established for this target.
    pub const NONE: Self = Self {
        durable_writes: false,
        atomic_put_if_absent: false,
        atomic_compare_exchange: false,
        read_after_write: false,
    };

    /// Guarantees required by a writable content publisher.
    pub const REQUIRED: Self = Self {
        durable_writes: true,
        atomic_put_if_absent: true,
        atomic_compare_exchange: true,
        read_after_write: true,
    };

    /// Rejects an adapter that cannot support safe write-through publication.
    pub fn validate_writable(self) -> Result<(), ConfigurationError> {
        for (provided, guarantee) in [
            (self.durable_writes, "durable writes"),
            (self.atomic_put_if_absent, "atomic put-if-absent"),
            (self.atomic_compare_exchange, "atomic compare-and-swap"),
            (self.read_after_write, "read-after-write"),
        ] {
            if !provided {
                return Err(ConfigurationError::MissingTargetGuarantee { guarantee });
            }
        }
        Ok(())
    }
}

/// Caller-provided exact object storage.
///
/// Methods take owned keys and bytes so returned futures do not borrow temporary
/// request data and remain straightforward to schedule on any executor.
pub trait TargetStore: Send + Sync {
    /// Adapter-specific definitive failure.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the target semantics available to the repository.
    fn guarantees(&self) -> TargetGuarantees;

    /// Reads one complete object and its opaque revision.
    ///
    /// The adapter must reject an object larger than `max_bytes` before downloading
    /// or allocating the oversized value.
    fn get(
        &self,
        key: ObjectKey,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Option<TargetObject>, Self::Error>> + Send;

    /// Reads exactly the requested half-open byte range.
    ///
    /// If the object exists but the exact range cannot be returned, the adapter
    /// must fail rather than silently clamp or return a short value.
    fn get_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send;

    /// Atomically and durably creates an immutable key if it is absent.
    ///
    /// The method must return [`PutIfAbsent::Ambiguous`] when a failure leaves
    /// commit status unknown. The caller owns exact readback resolution.
    fn put_if_absent(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<PutIfAbsent, Self::Error>> + Send;

    /// Atomically publishes bytes only if the opaque expected revision matches.
    ///
    /// `None` expects the key to be absent. The method must return
    /// [`CompareExchange::Ambiguous`] when a failure leaves commit status unknown.
    fn compare_exchange(
        &self,
        key: ObjectKey,
        expected: Option<ObjectVersion>,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<CompareExchange, Self::Error>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ranges_are_exact_and_checked() {
        let range = ObjectRange::new(2, 7).unwrap();
        assert_eq!(range.len(), 5);
        assert!(!range.is_empty());
        assert!(matches!(
            ObjectRange::new(7, 2),
            Err(RangeError::InvalidOrder { .. })
        ));
    }

    #[test]
    fn writable_guarantees_are_explicit() {
        assert_eq!(TargetGuarantees::REQUIRED.validate_writable(), Ok(()));
        let guarantees = TargetGuarantees {
            durable_writes: false,
            ..TargetGuarantees::REQUIRED
        };
        assert!(matches!(
            guarantees.validate_writable(),
            Err(ConfigurationError::MissingTargetGuarantee { .. })
        ));
    }
}
