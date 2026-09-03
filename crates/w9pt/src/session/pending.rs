//! Pending request correlation and exact completion matching.

use crate::{
    CancelKind, Completion,
    effect::PolicyResultKind,
    error::CompletionError,
    filesystem::FilesystemResultKind,
    protocol::{OperationId, ResponseBody, Tag},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedCompletion {
    Filesystem(FilesystemResultKind),
    Policy(PolicyResultKind),
}

impl ExpectedCompletion {
    pub fn validate(self, completion: &Completion) -> Result<(), CompletionError> {
        let operation_id = completion.operation_id();
        let valid = match (self, completion) {
            (
                Self::Filesystem(expected),
                Completion::Filesystem {
                    result: Ok(actual), ..
                },
            ) => expected == actual.kind(),
            (Self::Filesystem(_), Completion::Filesystem { result: Err(_), .. })
            | (
                Self::Filesystem(_),
                Completion::Cancelled {
                    kind: CancelKind::Filesystem,
                    ..
                },
            )
            | (Self::Policy(_), Completion::Policy { result: Err(_), .. })
            | (
                Self::Policy(_),
                Completion::Cancelled {
                    kind: CancelKind::Policy,
                    ..
                },
            ) => true,
            (
                Self::Policy(expected),
                Completion::Policy {
                    result: Ok(actual), ..
                },
            ) => expected == actual.kind(),
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(CompletionError::WrongKind {
                operation_id,
                expected: self.name(),
                actual: completion_kind_name(completion),
            })
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Filesystem(_) => "matching filesystem result or error",
            Self::Policy(_) => "matching policy result or error",
        }
    }
}

const fn completion_kind_name(completion: &Completion) -> &'static str {
    match completion {
        Completion::Filesystem { .. } => "filesystem completion",
        Completion::Policy { .. } => "policy completion",
        Completion::Cancelled {
            kind: CancelKind::Filesystem,
            ..
        } => "filesystem cancellation",
        Completion::Cancelled {
            kind: CancelKind::Policy,
            ..
        } => "policy cancellation",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Continuation {
    NoState(ResponseBody),
    AuthStart {
        fid: crate::protocol::Fid,
    },
    Attach {
        fid: crate::protocol::Fid,
    },
    AuthRead {
        maximum: u32,
    },
    AuthWrite {
        requested: u32,
    },
    AuthClunk {
        fid: crate::protocol::Fid,
    },
    Walk {
        source_fid: crate::protocol::Fid,
        new_fid: crate::protocol::Fid,
        source: super::fid::ObjectFid,
        requested: usize,
    },
    ReleaseFid {
        fid: crate::protocol::Fid,
    },
    RemoveFid {
        fid: crate::protocol::Fid,
    },
    Open {
        fid: crate::protocol::Fid,
        original: super::fid::ObjectFid,
    },
    Create {
        fid: crate::protocol::Fid,
        original: super::fid::ObjectFid,
    },
    Read {
        maximum: u32,
    },
    Write {
        requested: u32,
    },
    Statfs,
    Getattr,
    Setattr,
    Readlink,
    Readdir {
        maximum: u32,
    },
    Mkdir,
    Mknod,
    Symlink,
    XattrWalk {
        new_fid: crate::protocol::Fid,
        source: super::fid::ObjectFid,
    },
    XattrCreate {
        fid: crate::protocol::Fid,
        original: super::fid::ObjectFid,
        expected_size: u64,
    },
    XattrRead {
        maximum: u32,
    },
    XattrWrite {
        fid: crate::protocol::Fid,
        original: super::fid::ObjectFid,
        requested: u32,
    },
    XattrCommitFid {
        fid: crate::protocol::Fid,
    },
    XattrAbortFid {
        fid: crate::protocol::Fid,
        errno: crate::filesystem::LinuxErrno,
    },
    Fsync,
    Lock,
    Getlock,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingOperation {
    pub operation_id: OperationId,
    pub expected: ExpectedCompletion,
    pub kind: CancelKind,
    pub continuation: Continuation,
    pub response_suppressed: bool,
    pub cancellation_requested: bool,
    pub flush_waiters: Vec<Tag>,
    pub write_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FlushState {
    pub response_suppressed: bool,
    pub waiters: Vec<Tag>,
}
