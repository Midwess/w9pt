//! Stock `9P2000.L` wire types and checked codecs.

mod codec;
mod flags;
mod frame;
mod message;
mod message_type;
mod structures;
mod types;

pub use codec::{decode_request, encode_directory_entries, encode_response, validate_frame};
pub use flags::{
    GetattrMask, LockFlags, LockStatus, LockType, OpenFlags, SetattrMask, UnlinkFlags, XattrFlags,
};
pub use frame::FrameDecoder;
pub use message::{Request, RequestBody, Response, ResponseBody};
pub use message_type::{DispatchClass, MessageType, OPERATION_MATRIX, OperationSpec};
pub use structures::{
    DirectoryEntry, FileAttributes, LinuxWireError, Lock, LockRequest, SetAttributes, Statfs,
    StructureError, Timestamp, UserIdentity, XattrName,
};
pub use types::{Fid, OperationId, OperationRoute, Qid, QidType, SessionId, Tag};

/// The only dialect negotiated by this crate.
pub const VERSION_9P2000_L: &str = "9P2000.L";

/// Header bytes before a message payload.
pub const HEADER_SIZE: usize = 7;

/// Conservative overhead used to cap read/write payload replies.
pub const IO_HEADER_SIZE: usize = 24;

/// Wire sentinel indicating that no numeric user ID was supplied.
pub const NO_NUMERIC_USER: u32 = u32::MAX;
