//! Checked revisions, time values, cookies, fences, and fingerprints.

use core::fmt;

macro_rules! nonzero_counter {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            /// Creates a nonzero persistent value.
            pub const fn new(value: u64) -> Result<Self, InvalidValue> {
                if value == 0 {
                    Err(InvalidValue::Zero {
                        field: stringify!($name),
                    })
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the persistent integer.
            pub const fn get(self) -> u64 {
                self.0
            }

            /// Allocates the next value without wrapping.
            pub const fn checked_next(self) -> Result<Self, CounterOverflow> {
                match self.0.checked_add(1) {
                    Some(value) => Ok(Self(value)),
                    None => Err(CounterOverflow {
                        field: stringify!($name),
                    }),
                }
            }
        }
    };
}

nonzero_counter!(
    StateRevision,
    "Monotonic revision of one authoritative filesystem state."
);
nonzero_counter!(
    RecordRevision,
    "Monotonic revision assigned to an authoritative record."
);
nonzero_counter!(
    FencingToken,
    "Monotonic token that rejects stale state writers."
);
nonzero_counter!(
    InodeGeneration,
    "Monotonic generation of inode metadata visible to clients."
);
nonzero_counter!(
    DataGeneration,
    "Monotonic generation of regular-file content."
);
nonzero_counter!(
    DirectoryGeneration,
    "Monotonic generation of one directory namespace."
);
nonzero_counter!(LockGeneration, "Monotonic generation of one lock record.");

/// Caller-defined monotonic boundary before which a mutation result must remain retained.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MutationRetention(u64);

impl MutationRetention {
    /// Creates a retention horizon. Zero is the initial horizon and is valid.
    pub const fn new(horizon: u64) -> Self {
        Self(horizon)
    }

    /// Returns the caller-defined horizon.
    pub const fn horizon(self) -> u64 {
        self.0
    }
}

/// Stable directory enumeration cursor; zero denotes the beginning of a scan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DirectoryCookie(u64);

impl DirectoryCookie {
    /// Cursor used before the first directory entry.
    pub const START: Self = Self(0);

    /// Reconstructs a persisted directory cookie.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the persistent integer.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Allocates the next stable cookie without wrapping.
    pub const fn checked_next(self) -> Result<Self, CounterOverflow> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(CounterOverflow {
                field: "DirectoryCookie",
            }),
        }
    }

    /// Advances by an explicit nonzero allocation count without wrapping.
    pub const fn checked_advance(self, count: u64) -> Result<Self, CounterOverflow> {
        match self.0.checked_add(count) {
            Some(value) => Ok(Self(value)),
            None => Err(CounterOverflow {
                field: "DirectoryCookie",
            }),
        }
    }
}

/// Adapter-authoritative lease time expressed in abstract ticks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseDeadline(u64);

impl LeaseDeadline {
    /// Creates a deadline from adapter-defined clock ticks.
    pub const fn new(ticks: u64) -> Self {
        Self(ticks)
    }

    /// Returns the adapter-defined clock ticks.
    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// Adds a checked lease duration.
    pub const fn checked_add(self, duration: LeaseDuration) -> Result<Self, CounterOverflow> {
        match self.0.checked_add(duration.ticks()) {
            Some(value) => Ok(Self(value)),
            None => Err(CounterOverflow {
                field: "LeaseDeadline",
            }),
        }
    }
}

/// Positive requested lease duration in adapter-defined ticks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseDuration(u64);

impl LeaseDuration {
    /// Creates a positive lease duration.
    pub const fn new(ticks: u64) -> Result<Self, InvalidValue> {
        if ticks == 0 {
            Err(InvalidValue::Zero {
                field: "LeaseDuration",
            })
        } else {
            Ok(Self(ticks))
        }
    }

    /// Returns the adapter-defined clock ticks.
    pub const fn ticks(self) -> u64 {
        self.0
    }
}

/// Exact caller-supplied filesystem timestamp.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UnixTimestamp {
    seconds: i64,
    nanoseconds: u32,
}

impl UnixTimestamp {
    /// Creates a timestamp, rejecting a fractional second outside `0..1_000_000_000`.
    pub const fn new(seconds: i64, nanoseconds: u32) -> Result<Self, InvalidValue> {
        if nanoseconds >= 1_000_000_000 {
            Err(InvalidValue::Nanoseconds { nanoseconds })
        } else {
            Ok(Self {
                seconds,
                nanoseconds,
            })
        }
    }

    /// Returns whole seconds from the Unix epoch.
    pub const fn seconds(self) -> i64 {
        self.seconds
    }

    /// Returns the fractional nanoseconds.
    pub const fn nanoseconds(self) -> u32 {
        self.nanoseconds
    }
}

/// Digest of the complete semantic filesystem mutation request.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestFingerprint([u8; 32]);

impl RequestFingerprint {
    /// Reconstructs a persisted fingerprint.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Computes a BLAKE3-256 fingerprint of caller-canonical request bytes.
    pub fn blake3(canonical_request: &[u8]) -> Self {
        Self(*w9pt_storage::Digest::blake3(canonical_request).as_bytes())
    }

    /// Returns the canonical bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RequestFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestFingerprint(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

/// Invalid checked scalar value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidValue {
    /// A value required to be positive was zero.
    Zero {
        /// Stable field/type name.
        field: &'static str,
    },
    /// Timestamp fractional nanoseconds were outside their canonical range.
    Nanoseconds {
        /// Rejected fractional value.
        nanoseconds: u32,
    },
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => write!(formatter, "{field} must be nonzero"),
            Self::Nanoseconds { nanoseconds } => {
                write!(
                    formatter,
                    "nanoseconds {nanoseconds} are outside 0..1000000000"
                )
            }
        }
    }
}

impl std::error::Error for InvalidValue {}

/// A persistent monotonic value cannot be incremented without wrapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterOverflow {
    /// Stable field/type name.
    pub field: &'static str,
}

impl fmt::Display for CounterOverflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} exhausted its monotonic range", self.field)
    }
}

impl std::error::Error for CounterOverflow {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_values_never_wrap() {
        assert_eq!(
            StateRevision::new(0),
            Err(InvalidValue::Zero {
                field: "StateRevision"
            })
        );
        assert_eq!(
            StateRevision::new(1).unwrap().checked_next().unwrap().get(),
            2
        );
        assert!(
            StateRevision::new(u64::MAX)
                .unwrap()
                .checked_next()
                .is_err()
        );
        assert!(DirectoryCookie::new(u64::MAX).checked_next().is_err());
    }

    #[test]
    fn timestamp_and_lease_arithmetic_are_checked() {
        assert!(UnixTimestamp::new(1, 999_999_999).is_ok());
        assert!(UnixTimestamp::new(1, 1_000_000_000).is_err());
        assert_eq!(
            LeaseDeadline::new(7)
                .checked_add(LeaseDuration::new(5).unwrap())
                .unwrap()
                .ticks(),
            12
        );
        assert!(
            LeaseDeadline::new(u64::MAX)
                .checked_add(LeaseDuration::new(1).unwrap())
                .is_err()
        );
    }
}
