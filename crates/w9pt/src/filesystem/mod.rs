//! Backend-neutral filesystem requests, results, capabilities, and semantic errors.
//!
//! Requests contain opaque object/open/xattr handles and attach-bound principal/export context.
//! Implementations atomically authorize and perform mutations, coordinate stable identity,
//! open-unlinked objects, and locks across sessions, and honor every advertised [`Capability`].
//! Storage keys, extents, SDK values, transport bytes, and 9P encoding never cross this boundary.

mod capability;
mod error;
mod request;
mod response;
mod types;

pub use capability::{Capability, CapabilityError, CapabilitySet};
pub use error::{FilesystemError, LinuxErrno};
pub use request::{FilesystemOperation, FilesystemRequest};
pub use response::{
    AttachResult, CreateResult, FilesystemResult, FilesystemResultKind, NodeResult, OpenResult,
    WalkElement, WalkResult, XattrCreateResult, XattrWalkResult,
};
pub use types::{ExportId, ObjectHandle, OpenHandle, PrincipalId, RequestContext, XattrHandle};
