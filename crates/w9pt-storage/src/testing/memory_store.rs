//! Deterministic in-memory implementation of the target contract.

use core::{fmt, future::Future};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    CompareExchange, ObjectKey, ObjectRange, ObjectVersion, PutIfAbsent, TargetGuarantees,
    TargetObject, TargetOperation, TargetStore,
};

/// Whether an injected failure occurs before or after an operation's state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureTiming {
    /// Fail before observing or changing target state.
    Before,
    /// Fail after observing or changing target state.
    After,
}

/// Phase recorded for one deterministic target operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracePhase {
    /// The operation was accepted but has not acted on state yet.
    Before,
    /// The operation acted on target state.
    After,
}

/// One ordered target-operation trace event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetTraceEvent {
    /// Monotonic sequence number within this memory target.
    pub sequence: u64,
    /// Semantic target operation.
    pub operation: TargetOperation,
    /// Key used by the operation.
    pub key: ObjectKey,
    /// Before/after transition phase.
    pub phase: TracePhase,
}

/// Deterministic memory-target failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryTargetError {
    /// A configured failure point fired.
    Injected {
        /// Operation that reached the failure point.
        operation: TargetOperation,
        /// Whether state was acted on before failure.
        timing: FailureTiming,
    },
    /// An exact range extends beyond the stored object.
    RangeOutOfBounds {
        /// Stored object byte length.
        object_len: u64,
        /// Requested inclusive start.
        start: u64,
        /// Requested exclusive end.
        end: u64,
    },
    /// An exact object exceeds the caller-provided pre-allocation bound.
    ObjectTooLarge {
        /// Stored object byte length.
        object_len: u64,
        /// Caller-provided maximum byte length.
        max_bytes: u64,
    },
    /// Internal synchronization was poisoned by a panicking test thread.
    Poisoned,
}

impl fmt::Display for MemoryTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Injected { operation, timing } => {
                write!(formatter, "injected {timing:?} failure for {operation:?}")
            }
            Self::RangeOutOfBounds {
                object_len,
                start,
                end,
            } => write!(
                formatter,
                "exact range {start}..{end} exceeds object length {object_len}"
            ),
            Self::ObjectTooLarge {
                object_len,
                max_bytes,
            } => write!(
                formatter,
                "object length {object_len} exceeds exact-read bound {max_bytes}"
            ),
            Self::Poisoned => formatter.write_str("memory target synchronization poisoned"),
        }
    }
}

impl std::error::Error for MemoryTargetError {}

#[derive(Clone, Debug)]
struct StoredObject {
    bytes: Vec<u8>,
    version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FailurePoint {
    operation: TargetOperation,
    timing: FailureTiming,
    key: Option<ObjectKey>,
}

#[derive(Debug)]
struct State {
    objects: BTreeMap<ObjectKey, StoredObject>,
    next_version: u64,
    next_sequence: u64,
    trace: Vec<TargetTraceEvent>,
    failures: Vec<FailurePoint>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_version: 1,
            next_sequence: 0,
            trace: Vec::new(),
            failures: Vec::new(),
        }
    }
}

impl State {
    fn record(&mut self, operation: TargetOperation, key: &ObjectKey, phase: TracePhase) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.trace.push(TargetTraceEvent {
            sequence,
            operation,
            key: key.clone(),
            phase,
        });
    }

    fn should_fail(
        &mut self,
        operation: TargetOperation,
        timing: FailureTiming,
        key: &ObjectKey,
    ) -> bool {
        let Some(index) = self.failures.iter().position(|point| {
            point.operation == operation
                && point.timing == timing
                && point.key.as_ref().is_none_or(|expected| expected == key)
        }) else {
            return false;
        };
        self.failures.remove(index);
        true
    }

    fn allocate_version(&mut self) -> u64 {
        let version = self.next_version;
        self.next_version = self.next_version.saturating_add(1);
        version
    }
}

/// Cloneable deterministic target backed by one shared in-memory object map.
#[derive(Clone, Debug, Default)]
pub struct MemoryTarget {
    state: Arc<Mutex<State>>,
}

impl MemoryTarget {
    /// Creates an empty target.
    pub fn new() -> Self {
        Self::default()
    }

    /// Injects a one-shot failure at the next matching operation and timing.
    pub fn inject_failure(
        &self,
        operation: TargetOperation,
        timing: FailureTiming,
    ) -> Result<(), MemoryTargetError> {
        self.lock()?.failures.push(FailurePoint {
            operation,
            timing,
            key: None,
        });
        Ok(())
    }

    /// Injects a one-shot failure only for the next matching exact key.
    pub fn inject_failure_for(
        &self,
        operation: TargetOperation,
        timing: FailureTiming,
        key: ObjectKey,
    ) -> Result<(), MemoryTargetError> {
        self.lock()?.failures.push(FailurePoint {
            operation,
            timing,
            key: Some(key),
        });
        Ok(())
    }

    /// Returns all ordered trace events recorded so far.
    pub fn trace(&self) -> Result<Vec<TargetTraceEvent>, MemoryTargetError> {
        Ok(self.lock()?.trace.clone())
    }

    /// Clears operation tracing without modifying target objects or versions.
    pub fn clear_trace(&self) -> Result<(), MemoryTargetError> {
        self.lock()?.trace.clear();
        Ok(())
    }

    /// Returns exact stored bytes, bypassing normal tracing for test inspection.
    pub fn inspect(&self, key: &ObjectKey) -> Result<Option<Vec<u8>>, MemoryTargetError> {
        Ok(self
            .lock()?
            .objects
            .get(key)
            .map(|object| object.bytes.clone()))
    }

    /// Replaces bytes and revision outside the target contract to simulate corruption.
    pub fn corrupt(&self, key: &ObjectKey, bytes: Vec<u8>) -> Result<bool, MemoryTargetError> {
        let mut state = self.lock()?;
        let version = state.allocate_version();
        let Some(object) = state.objects.get_mut(key) else {
            return Ok(false);
        };
        object.bytes = bytes;
        object.version = version;
        Ok(true)
    }

    /// Returns the number of stored keys.
    pub fn object_count(&self) -> Result<usize, MemoryTargetError> {
        Ok(self.lock()?.objects.len())
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, MemoryTargetError> {
        self.state.lock().map_err(|_| MemoryTargetError::Poisoned)
    }
}

impl TargetStore for MemoryTarget {
    type Error = MemoryTargetError;

    fn guarantees(&self) -> TargetGuarantees {
        TargetGuarantees::REQUIRED
    }

    fn get(
        &self,
        key: ObjectKey,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Option<TargetObject>, Self::Error>> + Send {
        let target = self.clone();
        async move {
            let mut state = target.lock()?;
            state.record(TargetOperation::Get, &key, TracePhase::Before);
            if state.should_fail(TargetOperation::Get, FailureTiming::Before, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::Get,
                    timing: FailureTiming::Before,
                });
            }
            let result = match state.objects.get(&key) {
                Some(object) if object.bytes.len() > max_bytes => {
                    let object_len = u64::try_from(object.bytes.len()).unwrap_or(u64::MAX);
                    state.record(TargetOperation::Get, &key, TracePhase::After);
                    return Err(MemoryTargetError::ObjectTooLarge {
                        object_len,
                        max_bytes: u64::try_from(max_bytes).unwrap_or(u64::MAX),
                    });
                }
                Some(object) => Some(TargetObject::new(
                    object.bytes.clone(),
                    version_token(object.version),
                )),
                None => None,
            };
            state.record(TargetOperation::Get, &key, TracePhase::After);
            if state.should_fail(TargetOperation::Get, FailureTiming::After, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::Get,
                    timing: FailureTiming::After,
                });
            }
            Ok(result)
        }
    }

    fn get_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send {
        let target = self.clone();
        async move {
            let mut state = target.lock()?;
            state.record(TargetOperation::GetRange, &key, TracePhase::Before);
            if state.should_fail(TargetOperation::GetRange, FailureTiming::Before, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::GetRange,
                    timing: FailureTiming::Before,
                });
            }
            let result = match state.objects.get(&key) {
                Some(object) => {
                    let object_len = u64::try_from(object.bytes.len()).unwrap_or(u64::MAX);
                    if range.end() > object_len {
                        return Err(MemoryTargetError::RangeOutOfBounds {
                            object_len,
                            start: range.start(),
                            end: range.end(),
                        });
                    }
                    let start = usize::try_from(range.start()).map_err(|_| {
                        MemoryTargetError::RangeOutOfBounds {
                            object_len,
                            start: range.start(),
                            end: range.end(),
                        }
                    })?;
                    let end = usize::try_from(range.end()).map_err(|_| {
                        MemoryTargetError::RangeOutOfBounds {
                            object_len,
                            start: range.start(),
                            end: range.end(),
                        }
                    })?;
                    Some(object.bytes[start..end].to_vec())
                }
                None => None,
            };
            state.record(TargetOperation::GetRange, &key, TracePhase::After);
            if state.should_fail(TargetOperation::GetRange, FailureTiming::After, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::GetRange,
                    timing: FailureTiming::After,
                });
            }
            Ok(result)
        }
    }

    fn put_if_absent(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<PutIfAbsent, Self::Error>> + Send {
        let target = self.clone();
        async move {
            let mut state = target.lock()?;
            state.record(TargetOperation::PutIfAbsent, &key, TracePhase::Before);
            if state.should_fail(TargetOperation::PutIfAbsent, FailureTiming::Before, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::PutIfAbsent,
                    timing: FailureTiming::Before,
                });
            }
            let result = if let Some(object) = state.objects.get(&key) {
                PutIfAbsent::AlreadyExists {
                    version: version_token(object.version),
                }
            } else {
                let version = state.allocate_version();
                state
                    .objects
                    .insert(key.clone(), StoredObject { bytes, version });
                PutIfAbsent::Created {
                    version: version_token(version),
                }
            };
            state.record(TargetOperation::PutIfAbsent, &key, TracePhase::After);
            if state.should_fail(TargetOperation::PutIfAbsent, FailureTiming::After, &key) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::PutIfAbsent,
                    timing: FailureTiming::After,
                });
            }
            Ok(result)
        }
    }

    fn compare_exchange(
        &self,
        key: ObjectKey,
        expected: Option<ObjectVersion>,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<CompareExchange, Self::Error>> + Send {
        let target = self.clone();
        async move {
            let mut state = target.lock()?;
            state.record(TargetOperation::CompareExchange, &key, TracePhase::Before);
            if state.should_fail(
                TargetOperation::CompareExchange,
                FailureTiming::Before,
                &key,
            ) {
                return Err(MemoryTargetError::Injected {
                    operation: TargetOperation::CompareExchange,
                    timing: FailureTiming::Before,
                });
            }

            let current = state
                .objects
                .get(&key)
                .map(|object| version_token(object.version));
            let result = if current == expected {
                let version = state.allocate_version();
                state
                    .objects
                    .insert(key.clone(), StoredObject { bytes, version });
                CompareExchange::Replaced {
                    version: version_token(version),
                }
            } else {
                CompareExchange::Conflict { current }
            };
            state.record(TargetOperation::CompareExchange, &key, TracePhase::After);
            if state.should_fail(TargetOperation::CompareExchange, FailureTiming::After, &key) {
                return Ok(CompareExchange::Ambiguous);
            }
            Ok(result)
        }
    }
}

fn version_token(version: u64) -> ObjectVersion {
    ObjectVersion::new(version.to_be_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::block_on;

    fn key(value: &str) -> ObjectKey {
        ObjectKey::new(value).unwrap()
    }

    #[test]
    fn immutable_put_versions_and_exact_ranges_are_deterministic() {
        let target = MemoryTarget::new();
        let object_key = key("v1/data/a");
        let created =
            block_on(target.put_if_absent(object_key.clone(), b"abcdef".to_vec())).unwrap();
        let PutIfAbsent::Created { version } = created else {
            panic!("new key must be created");
        };

        assert_eq!(
            block_on(target.put_if_absent(object_key.clone(), b"other".to_vec())).unwrap(),
            PutIfAbsent::AlreadyExists {
                version: version.clone()
            }
        );
        assert_eq!(
            block_on(target.get_range(object_key.clone(), ObjectRange::new(1, 4).unwrap()))
                .unwrap(),
            Some(b"bcd".to_vec())
        );
        assert_eq!(
            block_on(target.get(object_key, 6))
                .unwrap()
                .unwrap()
                .bytes(),
            b"abcdef"
        );
    }

    #[test]
    fn cas_conflicts_and_after_failure_is_ambiguous_but_committed() {
        let target = MemoryTarget::new();
        let object_key = key("v1/refs/files/a");
        let first =
            block_on(target.compare_exchange(object_key.clone(), None, b"first".to_vec())).unwrap();
        let CompareExchange::Replaced { version } = first else {
            panic!("absent key must be created");
        };
        assert!(matches!(
            block_on(target.compare_exchange(object_key.clone(), None, b"lost".to_vec())).unwrap(),
            CompareExchange::Conflict { .. }
        ));

        target
            .inject_failure(TargetOperation::CompareExchange, FailureTiming::After)
            .unwrap();
        assert_eq!(
            block_on(target.compare_exchange(
                object_key.clone(),
                Some(version),
                b"second".to_vec(),
            ))
            .unwrap(),
            CompareExchange::Ambiguous
        );
        assert_eq!(
            block_on(target.get(object_key, 6))
                .unwrap()
                .unwrap()
                .bytes(),
            b"second"
        );
    }
}
