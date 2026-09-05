//! Bounded PostgreSQL polling of whole authoritative change events.

use sea_orm::DbErr;
use w9pt_fs_state::{
    AdapterFailureKind, ChangeBatch, ChangeCursor, ChangeEvent, ChangeOrigin, ChangePoll,
    ChangePollOutcome, FilesystemId, LeaseOperationId, StateLimits, StateRevision,
    StateStoreOperation,
};
use w9pt_fs_storage::MutationId;

use crate::{
    PostgresStateConfig, PostgresStateError,
    database::{PostgresConnection, PostgresTransaction, query},
    key_codec::SqlRecordKey,
    numeric::{decode_u64, encode_u64},
    sqlstate::SqlOperationPhase,
    transaction::{TransactionAccess, begin_transaction, commit_read_transaction},
};

const AUTHORITY_HEAD_SQL: &str = r#"
SELECT
    "current_revision"::text AS current_revision,
    "oldest_retained_revision"::text AS oldest_retained_revision
FROM "public"."w9pt_fs_state_authority_heads"
WHERE "filesystem_id" = $1
"#;

const CHANGE_HEADERS_SQL: &str = r#"
SELECT
    "revision"::text AS revision,
    "origin_kind",
    "origin_id",
    "key_count"
FROM "public"."w9pt_fs_state_change_commits" AS "commit_event"
WHERE "commit_event"."filesystem_id" = $1
  AND "commit_event"."revision" > $2::numeric
  AND "commit_event"."revision" <= $3::numeric
ORDER BY "commit_event"."revision"
LIMIT $4
"#;

const CHANGE_KEYS_SQL: &str = r#"
SELECT
    "revision"::text AS revision,
    "ordinal",
    "family_tag",
    "component_a",
    "component_b"
FROM "public"."w9pt_fs_state_change_keys" AS "commit_key"
WHERE "commit_key"."filesystem_id" = $1
  AND "commit_key"."revision" > $2::numeric
  AND "commit_key"."revision" <= $3::numeric
ORDER BY "commit_key"."revision", "commit_key"."ordinal"
LIMIT $4
"#;

/// Polls bounded, whole change events from one primary serializable snapshot.
pub(crate) async fn poll_changes(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    request: ChangePoll,
) -> Result<ChangePollOutcome, PostgresStateError> {
    let limits = config.limits();
    if let Err(error) = validate_receiver(request, limits) {
        return Ok(ChangePollOutcome::MalformedRequest(error));
    }

    let mut transaction = begin_transaction(
        pool,
        config,
        TransactionAccess::ReadOnly,
        StateStoreOperation::PollChanges,
    )
    .await?;
    let (current, oldest) = load_authority_head(&mut transaction, request.filesystem_id()).await?;
    let requested = request.after().revision();

    if requested > current {
        return finish_poll(
            transaction,
            ChangePollOutcome::RevisionUnavailable { requested, current },
        )
        .await;
    }
    if requested < oldest {
        return finish_poll(
            transaction,
            ChangePollOutcome::RevisionCompacted {
                oldest_available: oldest,
                current_revision: current,
            },
        )
        .await;
    }

    let headers = load_headers(&mut transaction, &request, current).await?;
    let selected = match plan_headers(&request, current, headers).map_err(corruption)? {
        HeaderPlan::PollBoundTooSmall {
            revision,
            required_keys,
        } => {
            return finish_poll(
                transaction,
                ChangePollOutcome::PollBoundTooSmall {
                    revision,
                    required_keys,
                },
            )
            .await;
        }
        HeaderPlan::Selected(headers) => headers,
    };

    let next = selected.last().map_or(request.after(), |header| {
        ChangeCursor::after(header.revision)
    });
    let expected_key_count = selected
        .iter()
        .try_fold(0u32, |total, header| total.checked_add(header.key_count));
    let expected_key_count =
        expected_key_count.ok_or_else(|| corruption("selected change key count overflowed u32"))?;
    let key_rows = match selected.last() {
        Some(header) => {
            load_key_rows(
                &mut transaction,
                request.filesystem_id(),
                request.after().revision(),
                header.revision,
                expected_key_count,
                limits,
            )
            .await?
        }
        None => Vec::new(),
    };
    let events = assemble_events(request.filesystem_id(), &selected, key_rows, limits)
        .map_err(corruption)?;
    let batch = ChangeBatch::new(events, &request, next, current, limits)
        .map_err(|error| corruption(format!("invalid decoded change batch: {error}")))?;
    finish_poll(transaction, ChangePollOutcome::Changes(batch)).await
}

fn validate_receiver(
    request: ChangePoll,
    limits: StateLimits,
) -> Result<(), w9pt_fs_state::InvalidChangeRequest> {
    ChangePoll::new(
        request.filesystem_id(),
        request.after(),
        request.max_events(),
        request.max_keys(),
        limits,
    )
    .map(|_| ())
}

async fn load_authority_head(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
) -> Result<(StateRevision, StateRevision), PostgresStateError> {
    let row = query(AUTHORITY_HEAD_SQL)
        .bind(filesystem_id.as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(statement_error)?;
    let Some(row) = row else {
        let baseline = nonzero_revision("revision-one authority baseline", 1)?;
        return Ok((baseline, baseline));
    };
    let current_text: String = row.try_get("current_revision").map_err(decode_error)?;
    let oldest_text: String = row
        .try_get("oldest_retained_revision")
        .map_err(decode_error)?;
    let current = decode_revision("current_revision", &current_text)?;
    let oldest = decode_revision("oldest_retained_revision", &oldest_text)?;
    if oldest > current {
        return Err(corruption(
            "oldest retained change revision exceeds current authority revision",
        ));
    }
    Ok((current, oldest))
}

async fn load_headers(
    transaction: &mut PostgresTransaction,
    request: &ChangePoll,
    current: StateRevision,
) -> Result<Vec<ChangeHeader>, PostgresStateError> {
    let rows = query(CHANGE_HEADERS_SQL)
        .bind(request.filesystem_id().as_bytes().to_vec())
        .bind(encode_u64(request.after().revision().get()))
        .bind(encode_u64(current.get()))
        .bind(i64::from(request.max_events()))
        .fetch_all(transaction)
        .await
        .map_err(statement_error)?;
    rows.into_iter()
        .map(|row| {
            let revision: String = row.try_get("revision").map_err(decode_error)?;
            let origin_kind: i16 = row.try_get("origin_kind").map_err(decode_error)?;
            let origin_id: Vec<u8> = row.try_get("origin_id").map_err(decode_error)?;
            let key_count: i32 = row.try_get("key_count").map_err(decode_error)?;
            decode_header(&revision, origin_kind, origin_id, key_count).map_err(corruption)
        })
        .collect()
}

async fn load_key_rows(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    after: StateRevision,
    through: StateRevision,
    expected_key_count: u32,
    limits: StateLimits,
) -> Result<Vec<ChangeKeyRow>, PostgresStateError> {
    let row_limit = i64::from(expected_key_count) + 1;
    let rows = query(CHANGE_KEYS_SQL)
        .bind(filesystem_id.as_bytes().to_vec())
        .bind(encode_u64(after.get()))
        .bind(encode_u64(through.get()))
        .bind(row_limit)
        .fetch_all(transaction)
        .await
        .map_err(statement_error)?;
    rows.into_iter()
        .map(|row| {
            let revision: String = row.try_get("revision").map_err(decode_error)?;
            let ordinal: i32 = row.try_get("ordinal").map_err(decode_error)?;
            let family_tag: i16 = row.try_get("family_tag").map_err(decode_error)?;
            let component_a: Option<Vec<u8>> = row.try_get("component_a").map_err(decode_error)?;
            let component_b: Option<Vec<u8>> = row.try_get("component_b").map_err(decode_error)?;
            decode_key_row(
                filesystem_id,
                &revision,
                ordinal,
                family_tag,
                component_a,
                component_b,
                limits,
            )
            .map_err(corruption)
        })
        .collect()
}

async fn finish_poll(
    transaction: PostgresTransaction,
    outcome: ChangePollOutcome,
) -> Result<ChangePollOutcome, PostgresStateError> {
    commit_read_transaction(transaction, StateStoreOperation::PollChanges).await?;
    Ok(outcome)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChangeHeader {
    revision: StateRevision,
    origin: ChangeOrigin,
    key_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChangeKeyRow {
    revision: StateRevision,
    ordinal: u32,
    key: w9pt_fs_state::RecordKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HeaderPlan {
    Selected(Vec<ChangeHeader>),
    PollBoundTooSmall {
        revision: StateRevision,
        required_keys: u32,
    },
}

fn decode_header(
    revision: &str,
    origin_kind: i16,
    origin_id: Vec<u8>,
    key_count: i32,
) -> Result<ChangeHeader, String> {
    let revision = decode_revision_value("change_commits.revision", revision)?;
    let origin_id = fixed_id("change_commits.origin_id", origin_id)?;
    let origin = match origin_kind {
        1 => ChangeOrigin::Mutation(MutationId::new(origin_id)),
        2 => ChangeOrigin::Lease(LeaseOperationId::new(origin_id)),
        _ => return Err(format!("unknown change origin tag {origin_kind}")),
    };
    let key_count = u32::try_from(key_count)
        .ok()
        .filter(|count| *count != 0)
        .ok_or_else(|| format!("invalid change key count {key_count}"))?;
    Ok(ChangeHeader {
        revision,
        origin,
        key_count,
    })
}

fn decode_key_row(
    filesystem_id: FilesystemId,
    revision: &str,
    ordinal: i32,
    family_tag: i16,
    component_a: Option<Vec<u8>>,
    component_b: Option<Vec<u8>>,
    limits: StateLimits,
) -> Result<ChangeKeyRow, String> {
    let revision = decode_revision_value("change_keys.revision", revision)?;
    let ordinal =
        u32::try_from(ordinal).map_err(|_| format!("negative change-key ordinal {ordinal}"))?;
    let key = SqlRecordKey {
        family_tag,
        filesystem_id: *filesystem_id.as_bytes(),
        component_a: component_a.unwrap_or_default(),
        component_b: component_b.unwrap_or_default(),
    }
    .decode(limits)
    .map_err(|error| format!("invalid persisted change key: {error}"))?;
    Ok(ChangeKeyRow {
        revision,
        ordinal,
        key,
    })
}

fn plan_headers(
    request: &ChangePoll,
    current: StateRevision,
    headers: Vec<ChangeHeader>,
) -> Result<HeaderPlan, String> {
    let maximum_events = usize::try_from(request.max_events()).unwrap_or(usize::MAX);
    if headers.len() > maximum_events {
        return Err("change header query exceeded the requested event bound".to_owned());
    }
    let mut selected = Vec::with_capacity(headers.len());
    let mut total_keys = 0u32;
    let mut previous = request.after().revision();
    for header in headers {
        let expected = previous
            .checked_next()
            .map_err(|_| "change revision overflow while checking history".to_owned())?;
        if header.revision != expected || header.revision > current {
            return Err(format!(
                "noncontiguous change history after revision {}: found {}",
                previous.get(),
                header.revision.get()
            ));
        }
        let next_total = total_keys
            .checked_add(header.key_count)
            .ok_or_else(|| "aggregate change key count overflow".to_owned())?;
        if next_total > request.max_keys() {
            if selected.is_empty() {
                return Ok(HeaderPlan::PollBoundTooSmall {
                    revision: header.revision,
                    required_keys: header.key_count,
                });
            }
            return Ok(HeaderPlan::Selected(selected));
        }
        total_keys = next_total;
        previous = header.revision;
        selected.push(header);
    }
    if previous < current && selected.len() < maximum_events {
        return Err(format!(
            "change history ends at revision {} before current revision {}",
            previous.get(),
            current.get()
        ));
    }
    Ok(HeaderPlan::Selected(selected))
}

fn assemble_events(
    filesystem_id: FilesystemId,
    headers: &[ChangeHeader],
    rows: Vec<ChangeKeyRow>,
    limits: StateLimits,
) -> Result<Vec<ChangeEvent>, String> {
    let mut rows = rows.into_iter();
    let mut events = Vec::with_capacity(headers.len());
    for header in headers {
        let capacity = usize::try_from(header.key_count).unwrap_or(usize::MAX);
        let mut keys = Vec::with_capacity(capacity);
        for expected_ordinal in 0..header.key_count {
            let row = rows.next().ok_or_else(|| {
                format!(
                    "change revision {} has fewer than {} keys",
                    header.revision.get(),
                    header.key_count
                )
            })?;
            if row.revision != header.revision || row.ordinal != expected_ordinal {
                return Err(format!(
                    "unexpected change key at revision {} ordinal {}",
                    row.revision.get(),
                    row.ordinal
                ));
            }
            keys.push(row.key);
        }
        let event = ChangeEvent::new(filesystem_id, header.revision, header.origin, keys, limits)
            .map_err(|error| format!("invalid decoded change event: {error}"))?;
        events.push(event);
    }
    if rows.next().is_some() {
        return Err("change-key query returned rows beyond selected headers".to_owned());
    }
    Ok(events)
}

fn decode_revision(field: &'static str, value: &str) -> Result<StateRevision, PostgresStateError> {
    let value = decode_u64(field, value)
        .map_err(|error| corruption(format!("invalid persisted revision: {error}")))?;
    nonzero_revision(field, value)
}

fn decode_revision_value(field: &'static str, value: &str) -> Result<StateRevision, String> {
    let value = decode_u64(field, value).map_err(|error| error.to_string())?;
    StateRevision::new(value).map_err(|error| error.to_string())
}

fn nonzero_revision(field: &'static str, value: u64) -> Result<StateRevision, PostgresStateError> {
    StateRevision::new(value).map_err(|error| corruption(format!("invalid {field}: {error}")))
}

fn fixed_id(field: &'static str, value: Vec<u8>) -> Result<[u8; 16], String> {
    let actual = value.len();
    value
        .try_into()
        .map_err(|_| format!("{field} has {actual} bytes, expected 16"))
}

fn statement_error(error: DbErr) -> PostgresStateError {
    PostgresStateError::from_database(
        StateStoreOperation::PollChanges,
        SqlOperationPhase::ExecuteStatement,
        error,
    )
}

fn decode_error(error: DbErr) -> PostgresStateError {
    PostgresStateError::from_database(
        StateStoreOperation::PollChanges,
        SqlOperationPhase::DecodeRow,
        error,
    )
}

fn corruption(detail: impl Into<String>) -> PostgresStateError {
    PostgresStateError::new(
        StateStoreOperation::PollChanges,
        AdapterFailureKind::Corruption,
        detail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_state::{InodeId, RecordKey, StateLimitValues};

    fn revision(value: u64) -> StateRevision {
        StateRevision::new(value).unwrap()
    }

    fn request(after: u64, max_events: u32, max_keys: u32) -> ChangePoll {
        ChangePoll::new(
            FilesystemId::from_u128(1),
            ChangeCursor::after(revision(after)),
            max_events,
            max_keys,
            StateLimits::default(),
        )
        .unwrap()
    }

    fn header(revision: u64, key_count: u32) -> ChangeHeader {
        ChangeHeader {
            revision: self::revision(revision),
            origin: ChangeOrigin::Mutation(MutationId::from_u128(u128::from(revision))),
            key_count,
        }
    }

    fn inode_row(revision: u64, ordinal: u32, inode: u128) -> ChangeKeyRow {
        ChangeKeyRow {
            revision: self::revision(revision),
            ordinal,
            key: RecordKey::Inode(FilesystemId::from_u128(1), InodeId::from_u128(inode)),
        }
    }

    #[test]
    fn receiver_revalidates_a_request_built_with_looser_limits() {
        let poll = request(1, 2, 3);
        let values = StateLimitValues {
            max_changes: 1,
            max_change_history_commits: 1,
            max_change_keys: 2,
            ..StateLimitValues::default()
        };
        let limits = StateLimits::new(values).unwrap();
        assert!(validate_receiver(poll, limits).is_err());
    }

    #[test]
    fn header_planning_obeys_whole_event_and_aggregate_key_bounds() {
        let poll = request(1, 3, 3);
        assert_eq!(
            plan_headers(&poll, revision(4), vec![header(2, 2), header(3, 2)]),
            Ok(HeaderPlan::Selected(vec![header(2, 2)]))
        );
        let too_small = request(1, 3, 1);
        assert_eq!(
            plan_headers(&too_small, revision(2), vec![header(2, 2)]),
            Ok(HeaderPlan::PollBoundTooSmall {
                revision: revision(2),
                required_keys: 2,
            })
        );
    }

    #[test]
    fn header_planning_detects_missing_or_unordered_history() {
        let poll = request(1, 3, 3);
        assert!(plan_headers(&poll, revision(3), vec![header(3, 1)]).is_err());
        assert!(plan_headers(&poll, revision(2), Vec::new()).is_err());
        assert!(plan_headers(&poll, revision(3), vec![header(2, 1)]).is_err());
    }

    #[test]
    fn exact_event_limit_leaves_resume_at_the_last_returned_revision() {
        let poll = request(1, 2, 4);
        assert_eq!(
            plan_headers(&poll, revision(4), vec![header(2, 1), header(3, 1)]),
            Ok(HeaderPlan::Selected(vec![header(2, 1), header(3, 1)]))
        );
    }

    #[test]
    fn origins_and_full_unsigned_revisions_decode_exactly() {
        let mutation = decode_header("18446744073709551615", 1, vec![1; 16], 1).unwrap();
        assert_eq!(mutation.revision.get(), u64::MAX);
        assert!(matches!(mutation.origin, ChangeOrigin::Mutation(_)));
        let lease = decode_header("2", 2, vec![2; 16], 1).unwrap();
        assert!(matches!(lease.origin, ChangeOrigin::Lease(_)));
        assert!(decode_header("0", 1, vec![1; 16], 1).is_err());
        assert!(decode_header("2", 3, vec![1; 16], 1).is_err());
        assert!(decode_header("2", 1, vec![1; 15], 1).is_err());
        assert!(decode_header("2", 1, vec![1; 16], 0).is_err());
    }

    #[test]
    fn event_assembly_requires_exact_revision_and_ordinal_rows() {
        let limits = StateLimits::default();
        let headers = vec![header(2, 2), header(3, 1)];
        let rows = vec![
            inode_row(2, 0, 10),
            inode_row(2, 1, 11),
            inode_row(3, 0, 12),
        ];
        let events = assemble_events(FilesystemId::from_u128(1), &headers, rows, limits).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].keys().len(), 2);
        assert_eq!(events[1].revision(), revision(3));

        assert!(
            assemble_events(
                FilesystemId::from_u128(1),
                &[header(2, 1)],
                vec![inode_row(2, 1, 10)],
                limits,
            )
            .is_err()
        );
        assert!(
            assemble_events(
                FilesystemId::from_u128(1),
                &[header(2, 1)],
                vec![inode_row(2, 0, 10), inode_row(2, 1, 11)],
                limits,
            )
            .is_err()
        );
    }

    #[test]
    fn key_rows_use_the_shared_semantic_key_codec() {
        let row = decode_key_row(
            FilesystemId::from_u128(1),
            "2",
            0,
            2,
            Some(InodeId::from_u128(9).as_bytes().to_vec()),
            None,
            StateLimits::default(),
        )
        .unwrap();
        assert_eq!(
            row.key,
            RecordKey::Inode(FilesystemId::from_u128(1), InodeId::from_u128(9))
        );
        assert!(
            decode_key_row(
                FilesystemId::from_u128(1),
                "2",
                -1,
                2,
                Some(vec![0; 16]),
                None,
                StateLimits::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn query_text_is_fully_qualified_and_keyset_ordered() {
        for query in [AUTHORITY_HEAD_SQL, CHANGE_HEADERS_SQL, CHANGE_KEYS_SQL] {
            assert!(query.contains("\"public\".\"w9pt_fs_state_"));
            assert!(!query.to_ascii_uppercase().contains("OFFSET"));
            assert!(!query.contains("has_more"));
        }
        assert!(CHANGE_HEADERS_SQL.contains("ORDER BY \"commit_event\".\"revision\""));
        assert!(CHANGE_HEADERS_SQL.contains("LIMIT $4"));
        assert!(
            CHANGE_KEYS_SQL
                .contains("ORDER BY \"commit_key\".\"revision\", \"commit_key\".\"ordinal\"")
        );
        assert!(CHANGE_KEYS_SQL.contains("LIMIT $4"));
    }
}
