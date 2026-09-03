//! Error domains for wire decoding, session state, host misuse, and terminal closure.

use core::fmt;

use crate::filesystem::LinuxErrno;
use crate::{
    limits::{InvalidLimits, LimitExceeded},
    protocol::OperationId,
};

/// Failure while validating or decoding untrusted wire bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum DecodeError {
    /// More bytes are needed to complete a stream value.
    Incomplete { needed: usize, available: usize },
    /// Declared frame is shorter than the seven-byte header.
    FrameTooSmall { size: u32 },
    /// Declared frame exceeds a configured or negotiated limit.
    FrameTooLarge { size: u32, maximum: u32 },
    /// Complete-frame input length disagrees with its prefix.
    LengthMismatch { declared: u32, actual: usize },
    /// Message number is not part of the declared operation matrix.
    UnknownMessageType(u8),
    /// A response-only message was supplied to the server decoder.
    UnexpectedResponse(u8),
    /// A length-prefixed string is not valid UTF-8.
    InvalidUtf8 { offset: usize },
    /// A decoded string exceeds the configured bound.
    StringTooLong { length: usize, maximum: usize },
    /// A decoded list exceeds its configured bound.
    CountTooLarge {
        kind: &'static str,
        count: usize,
        maximum: usize,
    },
    /// Checked wire-size arithmetic overflowed.
    ArithmeticOverflow,
    /// A scalar or composite value violates its protocol constraints.
    InvalidValue(&'static str),
    /// Payload bytes remained after decoding the declared message.
    TrailingBytes { remaining: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid 9P frame: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

/// Failure while encoding a typed response into a bounded frame.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum EncodeError {
    /// Encoded frame exceeds the active `msize`.
    FrameTooLarge { size: usize, maximum: u32 },
    /// String cannot be represented by its two-byte prefix.
    StringTooLong { length: usize, maximum: usize },
    /// List cannot be represented by its two-byte count.
    CountTooLarge {
        kind: &'static str,
        count: usize,
        maximum: usize,
    },
    /// Checked wire-size arithmetic overflowed.
    ArithmeticOverflow,
    /// Backend-supplied response value violates the wire contract.
    InvalidValue(&'static str),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "cannot encode 9P response: {self:?}")
    }
}

impl std::error::Error for EncodeError {}

/// Recoverable failure while driving a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    /// Session configuration is invalid.
    InvalidConfig(InvalidLimits),
    /// Input was rejected by the checked decoder.
    Decode(DecodeError),
    /// A typed response could not satisfy the active frame bound.
    Encode(EncodeError),
    /// Session is closing or closed and accepts no ordinary input.
    Closing,
    /// Retained session state reached an explicit bound.
    Limit(LimitExceeded),
    /// The session exhausted its never-reused 64-bit operation identifier space.
    OperationIdExhausted,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "session error: {self:?}")
    }
}

impl std::error::Error for SessionError {}

impl From<InvalidLimits> for SessionError {
    fn from(value: InvalidLimits) -> Self {
        Self::InvalidConfig(value)
    }
}

impl From<DecodeError> for SessionError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<EncodeError> for SessionError {
    fn from(value: EncodeError) -> Self {
        Self::Encode(value)
    }
}

impl From<LimitExceeded> for SessionError {
    fn from(value: LimitExceeded) -> Self {
        Self::Limit(value)
    }
}

/// Valid, tagged request failure caused by connection-scoped protocol state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStateError {
    /// Ordinary request arrived before successful `9P2000.L` negotiation.
    NotNegotiated,
    /// Request reused an active tag.
    DuplicateTag,
    /// Request used `NOTAG` outside version negotiation.
    InvalidTag,
    /// Fid does not exist in this session.
    UnknownFid,
    /// New fid already exists and replacement is not permitted.
    DuplicateFid,
    /// Operation requires an unopened fid.
    FidAlreadyOpen,
    /// Operation requires an open file/directory handle.
    FidNotOpen,
    /// Fid represents authentication or xattr state incompatible with the request.
    WrongFidKind,
    /// Attached export does not promise the required operation/semantics.
    Unsupported,
    /// Request scalar or state transition is invalid.
    InvalidRequest,
    /// A configured capacity limit prevents accepting more work.
    ResourceExhausted,
}

impl SessionStateError {
    /// Stable errno used in the request's `Rlerror` response.
    pub const fn errno(self) -> LinuxErrno {
        match self {
            Self::NotNegotiated => LinuxErrno::EPROTO,
            Self::DuplicateTag
            | Self::InvalidTag
            | Self::FidAlreadyOpen
            | Self::WrongFidKind
            | Self::InvalidRequest => LinuxErrno::EINVAL,
            Self::UnknownFid | Self::FidNotOpen => LinuxErrno::EBADF,
            Self::DuplicateFid => LinuxErrno::EEXIST,
            Self::Unsupported => LinuxErrno::EOPNOTSUPP,
            Self::ResourceExhausted => LinuxErrno::ENOMEM,
        }
    }
}

impl fmt::Display for SessionStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid session request state: {self:?}")
    }
}

impl std::error::Error for SessionStateError {}

/// Host misuse while supplying an external operation completion.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum CompletionError {
    /// No active operation has this identifier.
    UnknownOperation(OperationId),
    /// A previously terminal operation was completed again.
    DuplicateOperation(OperationId),
    /// Completion result kind differs from the emitted request kind.
    WrongKind {
        operation_id: OperationId,
        expected: &'static str,
        actual: &'static str,
    },
}

impl fmt::Display for CompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "completion error: {self:?}")
    }
}

impl std::error::Error for CompletionError {}

/// Reason the core requires its host to close the transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloseReason {
    /// Framing/message bytes cannot be safely associated with a request.
    MalformedInput(DecodeError),
    /// Host explicitly began shutdown.
    HostShutdown,
    /// Protocol state cannot continue safely.
    ProtocolViolation(&'static str),
    /// An invariant-preserving response cannot fit configured queue bounds.
    ResourceExhausted,
}
