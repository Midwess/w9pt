//! Session-scoped fid states.

use crate::{
    AuthHandle,
    filesystem::{CapabilitySet, ObjectHandle, OpenHandle, RequestContext, XattrHandle},
    protocol::Qid,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FidState {
    /// Fid number is claimed by an active auth/attach/walk transition.
    Pending,
    /// Host-defined protocol authentication byte stream.
    Auth { qid: Qid, handle: AuthHandle },
    /// Attached/resolved filesystem object.
    Object(ObjectFid),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectFid {
    pub object: ObjectHandle,
    pub qid: Qid,
    pub context: RequestContext,
    pub capabilities: CapabilitySet,
    pub open: Option<OpenHandle>,
    pub xattr: Option<XattrState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct XattrState {
    pub handle: XattrHandle,
    pub mode: XattrMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum XattrMode {
    Read { size: u64 },
    Write { expected_size: u64, written: u64 },
}
