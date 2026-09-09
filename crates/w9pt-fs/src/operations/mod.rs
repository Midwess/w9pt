//! Filesystem operation handlers.

mod attributes;
mod create;
mod link;
mod mutation;
mod node;
mod open;
mod read;
mod readdir;
mod readlink;
mod release;
mod rename;
mod setattr;
mod sync;
mod unlink;
mod walk;
mod write;

pub(crate) use attributes::execute_getattr;
pub(crate) use create::execute_create;
pub(crate) use link::execute_link;
pub(crate) use mutation::MutationOperationError;
pub(crate) use node::{execute_mkdir, execute_symlink};
pub(crate) use open::execute_open;
pub(crate) use read::{DataReadError, execute_read};
pub(crate) use readdir::execute_readdir;
pub(crate) use readlink::execute_readlink;
pub(crate) use release::execute_release;
pub(crate) use rename::execute_rename;
pub(crate) use setattr::execute_setattr;
pub(crate) use sync::execute_fsync;
pub(crate) use unlink::execute_unlink;
pub(crate) use walk::execute_walk;
pub(crate) use write::execute_write;

use core::fmt;

use w9pt::FilesystemError;
use w9pt_fs_state::StateLimitError;

/// Failure from a read-only semantic operation before outcome classification.
#[derive(Debug)]
pub(crate) enum ReadOperationError<S, P> {
    Client(FilesystemError),
    State(S),
    Policy(P),
    MalformedRead(StateLimitError),
    RevisionUnavailable,
    MalformedState,
}

impl<S: fmt::Display, P: fmt::Display> fmt::Display for ReadOperationError<S, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(formatter),
            Self::State(error) => write!(formatter, "state read failed: {error}"),
            Self::Policy(error) => write!(formatter, "export policy failed: {error}"),
            Self::MalformedRead(error) => error.fmt(formatter),
            Self::RevisionUnavailable => formatter.write_str("state revision unavailable"),
            Self::MalformedState => formatter.write_str("authoritative state result is malformed"),
        }
    }
}
