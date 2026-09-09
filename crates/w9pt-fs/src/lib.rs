//! Runtime-neutral filesystem semantics for `w9pt`.
//!
//! This crate coordinates owned filesystem operations with authoritative state
//! from `w9pt-fs-state` and immutable content from `w9pt-fs-storage`. Transport,
//! authentication, time, stable identities, database clients, object-store
//! clients, and task execution remain caller-owned.
//!
//! This crate is under active unreleased development. Its API and semantic
//! contracts may change directly without compatibility shims for earlier builds.

#![forbid(unsafe_code)]

mod authorization;
mod capability;
mod content_context;
mod engine;
mod error;
mod execution;
mod export;
mod filesystem_engine;
mod handles;
mod identity;
mod limits;
mod open_flags;
mod operations;
mod result_codec;
#[cfg(test)]
mod testing;

pub use authorization::{
    AccessRequirements, AuthorizationError, CreationAttributes, check_directory_mutation,
    check_directory_search, check_inode_access, check_mutation_allowed, check_owner_or_privileged,
    check_ownership_change, check_sticky_directory, creation_attributes,
};
pub use capability::derive_capabilities;
pub use content_context::{
    ContentContextOrchestrationError, content_context_create_changes, generate_content_context,
    load_committed_content_context, rewrap_content_context,
};
pub use engine::{
    LedgerProbeError, LedgerReplay, MutationRunnerError, PlannedCommitMismatch,
    probe_mutation_ledger, run_mutation,
};
pub use error::{
    AuthorityFailure, EngineProviderFailure, ExecutionContextError, ExecutionFailure,
    authorization_client_error, handle_client_error, runner_client_error,
};
pub use execution::{
    EngineOutcome, EngineRequest, EngineTerminal, ExecutionContext, UnresolvedEngineFailure,
};
pub use export::{
    CanonicalIdentity, ExportGrant, ExportPolicy, ExportPolicyRequest, IdentityMappingRequest,
    InvalidExportGrant, NumericIdentity, ReverseIdentityMappingRequest,
};
pub use filesystem_engine::{AttachResolutionError, FilesystemEngine};
pub use handles::{
    HandleResolutionError, inode_id_from_handle, object_handle, open_handle, open_id_from_handle,
    qid_from_inode, resolve_inode_record, resolve_open_record,
};
pub use identity::{IdentityScope, IdentitySource};
pub use limits::{
    EngineLimitError, EngineLimitKind, EngineLimitValues, EngineLimits, InvalidEngineLimits,
};
pub use open_flags::{OpenFlagError, OpenOptions, OpenPurpose, validate_open_flags};
pub use result_codec::{
    FingerprintError, ResultCodecError, decode_mutation_result, encode_mutation_result,
    mutation_fingerprint,
};
