//! A runtime-free, Sans-I/O server core for stock `9P2000.L`.
//!
//! A [`Session`] owns the protocol state for exactly one transport connection. The host feeds
//! bytes or complete frames into it, polls owned [`Effect`] values, performs the requested work,
//! and returns typed [`Completion`] values. This crate never opens sockets or files and never
//! starts an executor, thread, timer, or background task.
//!
//! This crate is under active unreleased development. Its Rust API has no
//! backward-compatibility guarantee, and no deprecated aliases are retained.
//!
//! ```no_run
//! use w9pt::{Effect, Session, SessionConfig, SessionContext, SessionId};
//!
//! let mut session = Session::new(
//!     SessionConfig::default(),
//!     SessionContext::new(SessionId::new(7)),
//! )?;
//!
//! session.receive_bytes(&[/* bytes read by the host */])?;
//! while let Some(effect) = session.poll_effect() {
//!     match effect {
//!         Effect::SendFrame { bytes } => {
//!             // The host writes the complete frame, preserving per-session effect order.
//!             let _ = bytes;
//!         }
//!         Effect::Filesystem { operation_id, request } => {
//!             // Perform `request`, then call `session.complete(...)` exactly once.
//!             let _ = (operation_id, request);
//!         }
//!         Effect::Policy { operation_id, request } => {
//!             let _ = (operation_id, request);
//!         }
//!         Effect::Cancel { operation_id, kind } => {
//!             let _ = (operation_id, kind);
//!         }
//!         Effect::CloseSession { reason } => {
//!             let _ = reason;
//!         }
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

pub mod config;
pub mod effect;
pub mod error;
pub mod filesystem;
pub mod limits;
pub mod protocol;
pub mod session;

pub use config::{SessionConfig, SessionContext};
pub use effect::{
    AuthHandle, CancelKind, Completion, CompletionKind, Effect, PolicyError, PolicyRequest,
    PolicyResult, PolicyResultKind,
};
pub use error::{
    CloseReason, CompletionError, DecodeError, EncodeError, SessionError, SessionStateError,
};
pub use filesystem::{FilesystemError, LinuxErrno};
pub use limits::{InvalidLimits, LimitExceeded, LimitKind, Limits};
pub use protocol::{Fid, OperationId, OperationRoute, Qid, SessionId, Tag};
pub use session::{Session, SessionStatus};
