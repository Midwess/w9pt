//! PostgreSQL SQLSTATE classification without localized message matching.

/// Adapter phase in which PostgreSQL reported an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SqlOperationPhase {
    /// Acquiring a pooled connection.
    AcquireConnection,
    /// Starting or configuring a transaction.
    BeginTransaction,
    /// Executing a statement before transaction completion.
    ExecuteStatement,
    /// Decoding an authoritative row returned by PostgreSQL.
    DecodeRow,
    /// Sending or awaiting `COMMIT`.
    Commit,
    /// Sending or awaiting `ROLLBACK`.
    Rollback,
}

/// A PostgreSQL abort that permits a bounded retry of the identical operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DefinitiveAbort {
    /// SQLSTATE `40001` serialization failure.
    SerializationFailure,
    /// SQLSTATE `40P01` deadlock detection.
    DeadlockDetected,
}

/// Required response to a recognized database constraint violation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConstraintAction {
    /// Roll back and resolve the operation through its durable replay ledger.
    ResolveLedger,
    /// Roll back and re-evaluate the semantic race from authoritative state.
    RecheckSemanticState,
    /// Treat the failure as an adapter/schema invariant violation.
    InvariantFailure,
}

/// One exact constraint name owned by the version-1 schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KnownConstraintViolation {
    name: &'static str,
    action: ConstraintAction,
}

impl KnownConstraintViolation {
    /// Returns the exact, locale-independent PostgreSQL constraint name.
    #[cfg(test)]
    pub(crate) const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the recovery action associated with this constraint.
    pub(crate) const fn action(self) -> ConstraintAction {
        self.action
    }
}

/// Actionable classification of a PostgreSQL database error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SqlFailureClass {
    /// Retry the exact same bounded transaction after a definitive abort.
    RetryIdentical(DefinitiveAbort),
    /// The operation exceeded an adapter-owned statement or lock deadline.
    Timeout,
    /// The connection or transaction was routed read-only.
    ReadOnly,
    /// PostgreSQL is unavailable, but no commit acknowledgement was in flight.
    Unavailable,
    /// `COMMIT` may have executed; resolve only through exact durable replay.
    AmbiguousCommit,
    /// PostgreSQL rejected an exact, recognized version-1 schema constraint.
    KnownConstraint(KnownConstraintViolation),
    /// Authoritative persisted state or a PostgreSQL structure is corrupt.
    Corruption,
    /// The failure is not a documented semantic or retry outcome.
    Internal,
}

/// Classifies one PostgreSQL SQLSTATE and optional constraint name.
///
/// The classifier deliberately accepts no server message text. Constraint
/// handling is an exact allowlist, and an error while `COMMIT` is in flight is
/// ambiguous unless its SQLSTATE establishes a definitive abort category.
pub(crate) fn classify_sqlstate(
    phase: SqlOperationPhase,
    sqlstate: &str,
    constraint: Option<&str>,
) -> SqlFailureClass {
    match sqlstate {
        "40001" => SqlFailureClass::RetryIdentical(DefinitiveAbort::SerializationFailure),
        "40P01" => SqlFailureClass::RetryIdentical(DefinitiveAbort::DeadlockDetected),
        "23505" => classify_constraint(constraint, UNIQUE_CONSTRAINTS),
        "23503" => classify_constraint(constraint, FOREIGN_KEY_CONSTRAINTS),
        "23514" => classify_constraint(constraint, CHECK_CONSTRAINTS),
        "22003" | "22P02" if phase == SqlOperationPhase::DecodeRow => SqlFailureClass::Corruption,
        "22003" | "22P02" => SqlFailureClass::Internal,
        "25006" => SqlFailureClass::ReadOnly,
        "55P03" | "57014" if phase == SqlOperationPhase::Commit => SqlFailureClass::AmbiguousCommit,
        "55P03" | "57014" => SqlFailureClass::Timeout,
        "XX001" | "XX002" => SqlFailureClass::Corruption,
        code if is_availability_code(code) && phase == SqlOperationPhase::Commit => {
            SqlFailureClass::AmbiguousCommit
        }
        code if is_availability_code(code) => SqlFailureClass::Unavailable,
        // An integrity violation not present in the versioned allowlist is an
        // adapter/schema mismatch. PostgreSQL definitively rejected the commit,
        // so it must not be mislabeled as ambiguous.
        code if code.starts_with("23") => SqlFailureClass::Internal,
        _ if phase == SqlOperationPhase::Commit => SqlFailureClass::AmbiguousCommit,
        _ => SqlFailureClass::Internal,
    }
}

fn classify_constraint(
    constraint: Option<&str>,
    known: &[(&'static str, ConstraintAction)],
) -> SqlFailureClass {
    let Some(constraint) = constraint else {
        return SqlFailureClass::Internal;
    };
    let Some(constraint) = constraint.strip_prefix("w9pt_fs_state_") else {
        return SqlFailureClass::Internal;
    };
    known
        .iter()
        .find_map(|&(name, action)| {
            (constraint == name).then_some(SqlFailureClass::KnownConstraint(
                KnownConstraintViolation { name, action },
            ))
        })
        .unwrap_or(SqlFailureClass::Internal)
}

fn is_availability_code(sqlstate: &str) -> bool {
    (sqlstate.len() == 5 && sqlstate.starts_with("08"))
        || matches!(sqlstate, "57P01" | "57P02" | "57P03" | "57P04" | "57P05")
}

const UNIQUE_CONSTRAINTS: &[(&str, ConstraintAction)] = &[
    ("schema_migrations_pkey", ConstraintAction::InvariantFailure),
    (
        "authority_heads_pkey",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "filesystem_records_pkey",
        ConstraintAction::RecheckSemanticState,
    ),
    ("inodes_pkey", ConstraintAction::RecheckSemanticState),
    (
        "content_metadata_pkey",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "content_metadata_context_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "content_metadata_owner_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "content_metadata_binding_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "inodes_qid_path_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "directory_entries_pkey",
        ConstraintAction::RecheckSemanticState,
    ),
    (
        "directory_entries_cookie_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    ("opens_pkey", ConstraintAction::RecheckSemanticState),
    (
        "opens_inode_identity_unique",
        ConstraintAction::RecheckSemanticState,
    ),
    ("open_pins_pkey", ConstraintAction::RecheckSemanticState),
    ("orphans_pkey", ConstraintAction::RecheckSemanticState),
    ("locks_pkey", ConstraintAction::RecheckSemanticState),
    ("xattrs_pkey", ConstraintAction::RecheckSemanticState),
    ("xattr_staging_pkey", ConstraintAction::RecheckSemanticState),
    ("mutation_results_pkey", ConstraintAction::ResolveLedger),
    ("writer_fences_pkey", ConstraintAction::RecheckSemanticState),
    (
        "writer_lease_operations_pkey",
        ConstraintAction::ResolveLedger,
    ),
    ("change_commits_pkey", ConstraintAction::InvariantFailure),
    ("change_keys_pkey", ConstraintAction::InvariantFailure),
];

const FOREIGN_KEY_CONSTRAINTS: &[(&str, ConstraintAction)] = &[
    (
        "directory_entries_parent_fk",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_child_fk",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_root_inode_fk",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_directory_parent_fk",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_context_fk",
        ConstraintAction::InvariantFailure,
    ),
    ("opens_inode_fk", ConstraintAction::InvariantFailure),
    ("open_pins_inode_fk", ConstraintAction::InvariantFailure),
    (
        "open_pins_open_inode_fk",
        ConstraintAction::InvariantFailure,
    ),
    ("orphans_inode_fk", ConstraintAction::InvariantFailure),
    ("locks_inode_fk", ConstraintAction::InvariantFailure),
    ("locks_owner_open_fk", ConstraintAction::InvariantFailure),
    ("xattrs_inode_fk", ConstraintAction::InvariantFailure),
    ("xattr_staging_inode_fk", ConstraintAction::InvariantFailure),
    ("change_keys_commit_fk", ConstraintAction::InvariantFailure),
];

const CHECK_CONSTRAINTS: &[(&str, ConstraintAction)] = &[
    (
        "schema_migrations_version_positive",
        ConstraintAction::InvariantFailure,
    ),
    (
        "schema_migrations_checksum_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "authority_heads_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "authority_heads_current_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "authority_heads_oldest_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "authority_heads_retention_order",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_root_inode_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_state_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_revision_match",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_next_qid_path_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_next_cookie_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "filesystem_records_policy_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    ("inodes_inode_id_width", ConstraintAction::InvariantFailure),
    ("inodes_qid_path_range", ConstraintAction::InvariantFailure),
    (
        "inodes_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    ("inodes_mode_range", ConstraintAction::InvariantFailure),
    ("inodes_owner_bounds", ConstraintAction::InvariantFailure),
    ("inodes_group_bounds", ConstraintAction::InvariantFailure),
    (
        "inodes_timestamp_nanoseconds_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_logical_size_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_link_count_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    ("inodes_kind_tag", ConstraintAction::InvariantFailure),
    ("inodes_owner_no_nul", ConstraintAction::InvariantFailure),
    ("inodes_group_no_nul", ConstraintAction::InvariantFailure),
    (
        "inodes_content_file_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_context_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_data_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_logical_size_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_manifest_key_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_manifest_digest_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_storage_method_tag",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_directory_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_directory_parent_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_directory_parent_shape",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_symlink_target_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_device_number_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_content_fields_all_or_none",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_regular_content_consistency",
        ConstraintAction::InvariantFailure,
    ),
    (
        "inodes_kind_specific_shape",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_content_file_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_owner_inode_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_context_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_policy_format_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_policy_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_key_shape",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_commitment_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_wrapped_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "content_metadata_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_parent_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_child_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_name_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_name_forbidden",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_cookie_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "directory_entries_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "opens_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    ("opens_open_id_width", ConstraintAction::InvariantFailure),
    ("opens_inode_id_width", ConstraintAction::InvariantFailure),
    (
        "opens_client_incarnation_width",
        ConstraintAction::InvariantFailure,
    ),
    ("opens_access_tag", ConstraintAction::InvariantFailure),
    (
        "opens_inode_generation_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "opens_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "open_pins_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "open_pins_inode_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "open_pins_open_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "open_pins_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "orphans_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    ("orphans_inode_id_width", ConstraintAction::InvariantFailure),
    (
        "orphans_open_pin_count_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "orphans_orphaned_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "orphans_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "locks_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    ("locks_inode_id_width", ConstraintAction::InvariantFailure),
    ("locks_lock_id_width", ConstraintAction::InvariantFailure),
    (
        "locks_owner_client_width",
        ConstraintAction::InvariantFailure,
    ),
    ("locks_owner_open_width", ConstraintAction::InvariantFailure),
    (
        "locks_range_start_range",
        ConstraintAction::InvariantFailure,
    ),
    ("locks_range_end_range", ConstraintAction::InvariantFailure),
    ("locks_range_nonempty", ConstraintAction::InvariantFailure),
    ("locks_kind_tag", ConstraintAction::InvariantFailure),
    ("locks_generation_range", ConstraintAction::InvariantFailure),
    (
        "locks_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattrs_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    ("xattrs_inode_id_width", ConstraintAction::InvariantFailure),
    ("xattrs_name_bounds", ConstraintAction::InvariantFailure),
    ("xattrs_name_no_nul", ConstraintAction::InvariantFailure),
    ("xattrs_value_bound", ConstraintAction::InvariantFailure),
    (
        "xattrs_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_staging_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_inode_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_name_bounds",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_name_no_nul",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_expected_size_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_bytes_bound",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_progress_bound",
        ConstraintAction::InvariantFailure,
    ),
    (
        "xattr_staging_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_mutation_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_fingerprint_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_client_incarnation_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_writer_scope_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_writer_incarnation_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_fencing_token_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_result_kind_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_result_format_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_result_bytes_bound",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_committed_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_retention_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_record_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "mutation_results_revision_match",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_scope_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_token_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_active_fields_all_or_none",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_active_holder_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_active_lease_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_active_deadline_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_fences_active_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_operation_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_kind_tag",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_fingerprint_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_outcome_tag",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_grant_all_or_none",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_grant_shape",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_scope_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_holder_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_lease_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_deadline_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "writer_lease_operations_token_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_commits_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_commits_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_commits_origin_kind_tag",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_commits_origin_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_commits_key_count_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_filesystem_id_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_revision_range",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_ordinal_range",
        ConstraintAction::InvariantFailure,
    ),
    ("change_keys_family_tag", ConstraintAction::InvariantFailure),
    (
        "change_keys_component_a_width",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_component_b_bound",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_component_shape",
        ConstraintAction::InvariantFailure,
    ),
    (
        "change_keys_component_b_shape",
        ConstraintAction::InvariantFailure,
    ),
    ("change_keys_name_shape", ConstraintAction::InvariantFailure),
];

#[cfg(test)]
mod tests {
    use super::*;

    const NON_COMMIT_PHASES: &[SqlOperationPhase] = &[
        SqlOperationPhase::AcquireConnection,
        SqlOperationPhase::BeginTransaction,
        SqlOperationPhase::ExecuteStatement,
        SqlOperationPhase::DecodeRow,
        SqlOperationPhase::Rollback,
    ];

    #[test]
    fn definitive_transaction_aborts_always_retry_identically() {
        for phase in NON_COMMIT_PHASES
            .iter()
            .copied()
            .chain([SqlOperationPhase::Commit])
        {
            assert_eq!(
                classify_sqlstate(phase, "40001", None),
                SqlFailureClass::RetryIdentical(DefinitiveAbort::SerializationFailure)
            );
            assert_eq!(
                classify_sqlstate(phase, "40P01", None),
                SqlFailureClass::RetryIdentical(DefinitiveAbort::DeadlockDetected)
            );
        }
    }

    #[test]
    fn timeouts_become_ambiguous_only_while_commit_is_in_flight() {
        for code in ["57014", "55P03"] {
            for phase in NON_COMMIT_PHASES {
                assert_eq!(
                    classify_sqlstate(*phase, code, None),
                    SqlFailureClass::Timeout
                );
            }
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::Commit, code, None),
                SqlFailureClass::AmbiguousCommit
            );
        }
    }

    #[test]
    fn read_only_is_explicit_in_every_phase() {
        for phase in NON_COMMIT_PHASES
            .iter()
            .copied()
            .chain([SqlOperationPhase::Commit])
        {
            assert_eq!(
                classify_sqlstate(phase, "25006", None),
                SqlFailureClass::ReadOnly
            );
        }
    }

    #[test]
    fn connection_and_shutdown_failures_respect_commit_ambiguity() {
        for code in ["08000", "08003", "08006", "08P01", "57P01", "57P05"] {
            for phase in NON_COMMIT_PHASES {
                assert_eq!(
                    classify_sqlstate(*phase, code, None),
                    SqlFailureClass::Unavailable
                );
            }
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::Commit, code, None),
                SqlFailureClass::AmbiguousCommit
            );
        }
    }

    #[test]
    fn malformed_connection_class_codes_are_not_availability() {
        for code in ["08", "080000", "08-message"] {
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::ExecuteStatement, code, None),
                SqlFailureClass::Internal
            );
        }
    }

    #[test]
    fn unknown_commit_errors_are_conservatively_ambiguous() {
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::Commit, "99999", None),
            SqlFailureClass::AmbiguousCommit
        );
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "99999", None),
            SqlFailureClass::Internal
        );
    }

    #[test]
    fn ledger_constraints_request_fresh_replay_resolution() {
        for name in ["mutation_results_pkey", "writer_lease_operations_pkey"] {
            let database_name = format!("w9pt_fs_state_{name}");
            let SqlFailureClass::KnownConstraint(violation) = classify_sqlstate(
                SqlOperationPhase::ExecuteStatement,
                "23505",
                Some(&database_name),
            ) else {
                panic!("expected known ledger constraint for {name}");
            };
            assert_eq!(violation.name(), name);
            assert_eq!(violation.action(), ConstraintAction::ResolveLedger);
        }
    }

    #[test]
    fn semantic_uniqueness_requests_authoritative_recheck() {
        for name in [
            "inodes_pkey",
            "directory_entries_pkey",
            "directory_entries_cookie_unique",
            "opens_inode_identity_unique",
        ] {
            let database_name = format!("w9pt_fs_state_{name}");
            let SqlFailureClass::KnownConstraint(violation) = classify_sqlstate(
                SqlOperationPhase::ExecuteStatement,
                "23505",
                Some(&database_name),
            ) else {
                panic!("expected known semantic constraint for {name}");
            };
            assert_eq!(violation.name(), name);
            assert_eq!(violation.action(), ConstraintAction::RecheckSemanticState);
        }
    }

    #[test]
    fn foreign_keys_and_checks_are_known_invariant_failures() {
        for (code, name) in [
            ("23503", "directory_entries_parent_fk"),
            ("23503", "change_keys_commit_fk"),
            ("23514", "inodes_kind_specific_shape"),
            ("23514", "writer_fences_active_fields_all_or_none"),
        ] {
            let database_name = format!("w9pt_fs_state_{name}");
            let SqlFailureClass::KnownConstraint(violation) = classify_sqlstate(
                SqlOperationPhase::ExecuteStatement,
                code,
                Some(&database_name),
            ) else {
                panic!("expected known invariant constraint for {name}");
            };
            assert_eq!(violation.name(), name);
            assert_eq!(violation.action(), ConstraintAction::InvariantFailure);
        }
    }

    #[test]
    fn missing_unknown_or_wrong_kind_constraints_are_internal() {
        for (code, constraint) in [
            ("23505", None),
            ("23505", Some("unrecognized_unique")),
            ("23503", Some("w9pt_fs_state_inodes_pkey")),
            ("23514", Some("w9pt_fs_state_directory_entries_parent_fk")),
            ("23502", Some("w9pt_fs_state_inodes_owner_bounds")),
        ] {
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::Commit, code, constraint),
                SqlFailureClass::Internal
            );
        }
    }

    #[test]
    fn numeric_conversion_errors_distinguish_persisted_corruption() {
        for code in ["22003", "22P02"] {
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::DecodeRow, code, None),
                SqlFailureClass::Corruption
            );
            assert_eq!(
                classify_sqlstate(SqlOperationPhase::ExecuteStatement, code, None),
                SqlFailureClass::Internal
            );
        }
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "XX001", None),
            SqlFailureClass::Corruption
        );
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "XX002", None),
            SqlFailureClass::Corruption
        );
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "XX000", None),
            SqlFailureClass::Internal
        );
    }

    #[test]
    fn every_explicit_migration_constraint_is_allowlisted_by_kind() {
        let mut classified = std::collections::BTreeSet::new();
        for (sqlstate, known) in [
            ("23505", UNIQUE_CONSTRAINTS),
            ("23503", FOREIGN_KEY_CONSTRAINTS),
            ("23514", CHECK_CONSTRAINTS),
        ] {
            for &(name, _) in known {
                let database_name = format!("w9pt_fs_state_{name}");
                assert!(matches!(
                    classify_sqlstate(
                        SqlOperationPhase::ExecuteStatement,
                        sqlstate,
                        Some(&database_name),
                    ),
                    SqlFailureClass::KnownConstraint(_)
                ));
                assert!(classified.insert(database_name));
            }
        }
        assert_eq!(
            classified,
            crate::schema::CONSTRAINT_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        );
    }

    #[test]
    fn classifier_has_no_message_input_or_message_fallback() {
        assert_eq!(
            classify_sqlstate(
                SqlOperationPhase::ExecuteStatement,
                "23505",
                Some("unknown_constraint")
            ),
            SqlFailureClass::Internal
        );
        assert_eq!(
            classify_sqlstate(
                SqlOperationPhase::ExecuteStatement,
                "99999",
                Some("w9pt_fs_state_mutation_results_pkey")
            ),
            SqlFailureClass::Internal
        );
    }
}
