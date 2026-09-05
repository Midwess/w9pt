//! Bounded whole-commit revision change polling.

use core::fmt;
use std::collections::BTreeSet;

use crate::{
    FilesystemId, LeaseOperationId, RecordKey, StateLimitError, StateLimitKind, StateLimits,
    StateRevision,
};

/// Stable operation that caused one authoritative revision event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChangeOrigin {
    /// Serializable filesystem mutation and its durable result-ledger identity.
    Mutation(w9pt_fs_storage::MutationId),
    /// Idempotent writer-lease operation.
    Lease(LeaseOperationId),
}

/// Stable exclusive revision cursor for nonblocking change polling.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChangeCursor(StateRevision);

impl ChangeCursor {
    /// Resumes strictly after the supplied observed revision.
    pub const fn after(revision: StateRevision) -> Self {
        Self(revision)
    }

    /// Returns the exclusive revision boundary.
    pub const fn revision(self) -> StateRevision {
        self.0
    }
}

/// Nonblocking bounded request for whole commits after one revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChangePoll {
    filesystem_id: FilesystemId,
    after: ChangeCursor,
    max_events: u32,
    max_keys: u32,
}

impl ChangePoll {
    /// Creates a nonzero poll request within contract bounds.
    pub fn new(
        filesystem_id: FilesystemId,
        after: ChangeCursor,
        max_events: u32,
        max_keys: u32,
        limits: StateLimits,
    ) -> Result<Self, InvalidChangeRequest> {
        if max_events == 0 {
            return Err(InvalidChangeRequest::ZeroEvents);
        }
        if max_keys == 0 {
            return Err(InvalidChangeRequest::ZeroKeys);
        }
        limits
            .require_count(
                StateLimitKind::ChangeHistory,
                usize::try_from(max_events).unwrap_or(usize::MAX),
                limits.max_change_history_commits(),
            )
            .map_err(InvalidChangeRequest::Limit)?;
        limits
            .require_count(
                StateLimitKind::ChangeKeys,
                usize::try_from(max_keys).unwrap_or(usize::MAX),
                limits.max_change_keys(),
            )
            .map_err(InvalidChangeRequest::Limit)?;
        Ok(Self {
            filesystem_id,
            after,
            max_events,
            max_keys,
        })
    }

    /// Returns the selected filesystem authority.
    pub const fn filesystem_id(self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the exclusive resume cursor.
    pub const fn after(self) -> ChangeCursor {
        self.after
    }

    /// Returns the maximum whole events in one response.
    pub const fn max_events(self) -> u32 {
        self.max_events
    }

    /// Returns the maximum aggregate changed keys in one response.
    pub const fn max_keys(self) -> u32 {
        self.max_keys
    }

    pub(crate) fn validate(self, limits: StateLimits) -> Result<(), InvalidChangeRequest> {
        Self::new(
            self.filesystem_id,
            self.after,
            self.max_events,
            self.max_keys,
            limits,
        )
        .map(|_| ())
    }
}

/// All semantic keys changed atomically at one revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeEvent {
    filesystem_id: FilesystemId,
    revision: StateRevision,
    origin: ChangeOrigin,
    keys: Box<[RecordKey]>,
}

impl ChangeEvent {
    /// Creates one nonempty bounded, deduplicated whole-commit event.
    pub fn new(
        filesystem_id: FilesystemId,
        revision: StateRevision,
        origin: ChangeOrigin,
        keys: impl Into<Vec<RecordKey>>,
        limits: StateLimits,
    ) -> Result<Self, InvalidChangeRequest> {
        let keys = keys.into();
        if keys.is_empty() {
            return Err(InvalidChangeRequest::EmptyEvent);
        }
        limits
            .require_count(
                StateLimitKind::ChangeKeys,
                keys.len(),
                limits.max_change_keys(),
            )
            .map_err(InvalidChangeRequest::Limit)?;
        let mut unique = BTreeSet::new();
        for key in &keys {
            if key.filesystem_id() != filesystem_id {
                return Err(InvalidChangeRequest::KeyOutsideFilesystem(key.clone()));
            }
            if !unique.insert(key.clone()) {
                return Err(InvalidChangeRequest::DuplicateKey(key.clone()));
            }
            key.validate_against_limits(limits)
                .map_err(InvalidChangeRequest::Limit)?;
        }
        Ok(Self {
            filesystem_id,
            revision,
            origin,
            keys: keys.into_boxed_slice(),
        })
    }

    /// Returns the filesystem whose authority changed.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns this complete commit's state revision.
    pub const fn revision(&self) -> StateRevision {
        self.revision
    }

    /// Returns the stable mutation or lease operation causing this revision.
    pub const fn origin(&self) -> ChangeOrigin {
        self.origin
    }

    /// Returns all requested semantic keys changed by the commit.
    pub fn keys(&self) -> &[RecordKey] {
        &self.keys
    }
}

/// Bounded nonblocking page of complete change events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeBatch {
    events: Box<[ChangeEvent]>,
    next: ChangeCursor,
    current_revision: StateRevision,
}

impl ChangeBatch {
    /// Creates an ordered response; an empty event vector is valid.
    pub fn new(
        events: impl Into<Vec<ChangeEvent>>,
        request: &ChangePoll,
        next: ChangeCursor,
        current_revision: StateRevision,
        limits: StateLimits,
    ) -> Result<Self, InvalidChangeRequest> {
        request.validate(limits)?;
        let events = events.into();
        limits
            .require_count(
                StateLimitKind::ChangeHistory,
                events.len(),
                request.max_events(),
            )
            .map_err(InvalidChangeRequest::Limit)?;
        let mut total_keys = 0usize;
        let mut previous = request.after().revision();
        for event in &events {
            if event.filesystem_id() != request.filesystem_id() {
                return Err(InvalidChangeRequest::EventOutsideFilesystem);
            }
            total_keys = total_keys
                .checked_add(event.keys().len())
                .ok_or(InvalidChangeRequest::KeyCountOverflow)?;
            for key in event.keys() {
                key.validate_against_limits(limits)
                    .map_err(InvalidChangeRequest::Limit)?;
            }
            limits
                .require_count(StateLimitKind::ChangeKeys, total_keys, request.max_keys())
                .map_err(InvalidChangeRequest::Limit)?;
            if event.revision() <= previous || event.revision() > current_revision {
                return Err(InvalidChangeRequest::UnorderedRevision {
                    previous,
                    next: event.revision(),
                });
            }
            previous = event.revision();
        }
        if next.revision() < previous || next.revision() > current_revision {
            return Err(InvalidChangeRequest::InvalidNextCursor {
                last_event: previous,
                next: next.revision(),
                current: current_revision,
            });
        }
        Ok(Self {
            events: events.into_boxed_slice(),
            next,
            current_revision,
        })
    }

    /// Returns complete events in strictly increasing revision order.
    pub fn events(&self) -> &[ChangeEvent] {
        &self.events
    }

    /// Returns the cursor for the next poll; unchanged for an empty batch.
    pub const fn next(&self) -> ChangeCursor {
        self.next
    }

    /// Returns the authority revision observed while polling.
    pub const fn current_revision(&self) -> StateRevision {
        self.current_revision
    }
}

/// Semantic outcome of change polling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangePollOutcome {
    /// Requested point remains in history and a bounded page was returned.
    Changes(ChangeBatch),
    /// Requested cursor is newer than the authority's current revision.
    RevisionUnavailable {
        /// Requested future revision.
        requested: StateRevision,
        /// Current authority revision.
        current: StateRevision,
    },
    /// Requested point predates retained history; callers must discard caches.
    RevisionCompacted {
        /// Oldest revision from which polling can safely resume.
        oldest_available: StateRevision,
        /// Current authority revision.
        current_revision: StateRevision,
    },
    /// The next whole event cannot fit without being split; increase key bound.
    PollBoundTooSmall {
        /// Next complete event revision.
        revision: StateRevision,
        /// Keys required to return it whole.
        required_keys: u32,
    },
    /// Request was built against looser bounds than the receiving adapter permits.
    MalformedRequest(InvalidChangeRequest),
}

/// Invalid change event, cursor, or polling bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvalidChangeRequest {
    /// Poll must permit at least one whole event.
    ZeroEvents,
    /// Poll must permit at least one changed key.
    ZeroKeys,
    /// A whole commit must change at least one requested semantic key.
    EmptyEvent,
    /// A configured bound was exceeded.
    Limit(StateLimitError),
    /// Changed key belongs to another filesystem.
    KeyOutsideFilesystem(RecordKey),
    /// Whole event contained one semantic key more than once.
    DuplicateKey(RecordKey),
    /// Events were not strictly increasing after the requested cursor.
    UnorderedRevision {
        /// Prior cursor/event revision.
        previous: StateRevision,
        /// Rejected next event revision.
        next: StateRevision,
    },
    /// Batch resume cursor did not cover its events or exceeded current state.
    InvalidNextCursor {
        /// Requested cursor or final event revision.
        last_event: StateRevision,
        /// Rejected response cursor.
        next: StateRevision,
        /// Current authority revision.
        current: StateRevision,
    },
    /// Batch contained an event for another filesystem.
    EventOutsideFilesystem,
    /// Aggregate event-key counting overflowed.
    KeyCountOverflow,
}

impl fmt::Display for InvalidChangeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroEvents => formatter.write_str("change poll event bound must be nonzero"),
            Self::ZeroKeys => formatter.write_str("change poll key bound must be nonzero"),
            Self::EmptyEvent => formatter.write_str("whole-commit change event is empty"),
            Self::Limit(error) => error.fmt(formatter),
            Self::KeyOutsideFilesystem(key) => {
                write!(
                    formatter,
                    "change key belongs to another filesystem: {key:?}"
                )
            }
            Self::DuplicateKey(key) => write!(formatter, "duplicate change key: {key:?}"),
            Self::UnorderedRevision { previous, next } => write!(
                formatter,
                "change revision {} does not follow {}",
                next.get(),
                previous.get()
            ),
            Self::InvalidNextCursor {
                last_event,
                next,
                current,
            } => write!(
                formatter,
                "change cursor {} does not cover {} through current revision {}",
                next.get(),
                last_event.get(),
                current.get()
            ),
            Self::EventOutsideFilesystem => {
                formatter.write_str("change batch contains an event for another filesystem")
            }
            Self::KeyCountOverflow => formatter.write_str("change batch key count overflowed"),
        }
    }
}

impl std::error::Error for InvalidChangeRequest {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_poll_batches_keep_the_resume_cursor() {
        let cursor = ChangeCursor::after(StateRevision::new(3).unwrap());
        let limits = StateLimits::default();
        let request = ChangePoll::new(FilesystemId::from_u128(1), cursor, 1, 1, limits).unwrap();
        let batch = ChangeBatch::new(
            vec![],
            &request,
            cursor,
            StateRevision::new(5).unwrap(),
            limits,
        )
        .unwrap();
        assert!(batch.events().is_empty());
        assert_eq!(batch.next(), cursor);
        assert_eq!(batch.current_revision().get(), 5);
    }

    #[test]
    fn whole_events_are_bounded_owned_and_strictly_ordered() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let event = ChangeEvent::new(
            filesystem_id,
            StateRevision::new(2).unwrap(),
            ChangeOrigin::Mutation(w9pt_fs_storage::MutationId::from_u128(4)),
            vec![RecordKey::Inode(
                filesystem_id,
                crate::InodeId::from_u128(3),
            )],
            limits,
        )
        .unwrap();
        let cursor = ChangeCursor::after(StateRevision::new(1).unwrap());
        let request = ChangePoll::new(filesystem_id, cursor, 1, 1, limits).unwrap();
        let batch = ChangeBatch::new(
            vec![event],
            &request,
            ChangeCursor::after(StateRevision::new(2).unwrap()),
            StateRevision::new(2).unwrap(),
            limits,
        )
        .unwrap();
        assert_eq!(batch.next().revision().get(), 2);
        assert!(ChangePoll::new(filesystem_id, cursor, 0, 1, limits).is_err());
    }

    #[test]
    fn events_and_batches_revalidate_keys_poll_bounds_and_filesystem() {
        let limits = StateLimits::default();
        let first = FilesystemId::from_u128(1);
        let second = FilesystemId::from_u128(2);
        let cursor = ChangeCursor::after(StateRevision::new(1).unwrap());
        let request = ChangePoll::new(first, cursor, 1, 1, limits).unwrap();
        let event = ChangeEvent::new(
            second,
            StateRevision::new(2).unwrap(),
            ChangeOrigin::Mutation(w9pt_fs_storage::MutationId::from_u128(3)),
            vec![RecordKey::Inode(second, crate::InodeId::from_u128(4))],
            limits,
        )
        .unwrap();
        assert_eq!(
            ChangeBatch::new(
                vec![event],
                &request,
                ChangeCursor::after(StateRevision::new(2).unwrap()),
                StateRevision::new(2).unwrap(),
                limits,
            ),
            Err(InvalidChangeRequest::EventOutsideFilesystem)
        );

        let strict = StateLimits::new(crate::StateLimitValues {
            max_entry_name_bytes: 1,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let loose_name = crate::EntryName::new(b"long".to_vec(), limits).unwrap();
        let loose_event = ChangeEvent::new(
            first,
            StateRevision::new(2).unwrap(),
            ChangeOrigin::Mutation(w9pt_fs_storage::MutationId::from_u128(5)),
            vec![RecordKey::DirectoryEntry(
                first,
                crate::InodeId::from_u128(6),
                loose_name.clone(),
            )],
            limits,
        )
        .unwrap();
        assert!(matches!(
            ChangeBatch::new(
                vec![loose_event],
                &request,
                ChangeCursor::after(StateRevision::new(2).unwrap()),
                StateRevision::new(2).unwrap(),
                strict,
            ),
            Err(InvalidChangeRequest::Limit(StateLimitError {
                kind: StateLimitKind::EntryName,
                ..
            }))
        ));
        assert!(matches!(
            ChangeEvent::new(
                first,
                StateRevision::new(2).unwrap(),
                ChangeOrigin::Mutation(w9pt_fs_storage::MutationId::from_u128(7)),
                vec![RecordKey::DirectoryEntry(
                    first,
                    crate::InodeId::from_u128(8),
                    loose_name,
                )],
                strict,
            ),
            Err(InvalidChangeRequest::Limit(StateLimitError {
                kind: StateLimitKind::EntryName,
                ..
            }))
        ));
    }
}
