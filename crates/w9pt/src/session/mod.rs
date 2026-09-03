//! The independently owned Sans-I/O connection state machine.
//!
//! One [`Session`] belongs to one transport connection. It has no shared singleton, executor,
//! mutex, background task, or destructor I/O. Hosts route effects by session/operation identity,
//! preserve `SendFrame` polling order, and explicitly drive [`Session::begin_close`] to
//! [`SessionStatus::Drained`] when backend cleanup is required.

mod fid;
mod pending;

use std::collections::{HashMap, VecDeque};

use crate::{
    CancelKind, Completion, Effect, PolicyRequest, PolicyResult, SessionConfig, SessionContext,
    effect::OperationIdAllocator,
    error::{CloseReason, CompletionError, DecodeError, SessionError, SessionStateError},
    filesystem::{
        AttachResult, CapabilitySet, FilesystemOperation, FilesystemRequest, FilesystemResult,
        LinuxErrno, RequestContext, WalkResult,
    },
    limits::{Accounting, LimitKind},
    protocol::{
        Fid, FrameDecoder, IO_HEADER_SIZE, LinuxWireError, OperationId, Request, RequestBody,
        Response, ResponseBody, Tag, UserIdentity, VERSION_9P2000_L, decode_request,
        encode_directory_entries, encode_response,
    },
};

use fid::{FidState, ObjectFid, XattrMode, XattrState};
use pending::{Continuation, ExpectedCompletion, FlushState, PendingOperation};

#[derive(Clone, Debug)]
enum CleanupAction {
    Cancel {
        operation_id: OperationId,
        kind: CancelKind,
    },
    Policy(PolicyRequest),
    Filesystem(FilesystemRequest),
}

/// Observable lifecycle state of one connection state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
    /// Accepting transport input and completions.
    Active,
    /// No new ordinary input is accepted; terminal cleanup/effects may remain.
    Closing,
    /// All tracked work/effects are drained after explicit closure.
    Drained,
}

/// Independently owned Sans-I/O protocol state for one transport connection.
#[derive(Debug)]
pub struct Session {
    config: SessionConfig,
    context: SessionContext,
    decoder: FrameDecoder,
    negotiated_msize: Option<u32>,
    effects: VecDeque<Effect>,
    accounting: Accounting,
    operation_ids: OperationIdAllocator,
    pending_by_tag: HashMap<Tag, PendingOperation>,
    flushes: HashMap<Tag, FlushState>,
    tag_by_operation: HashMap<OperationId, Tag>,
    abandoned_operations: HashMap<OperationId, ExpectedCompletion>,
    last_issued_operation_id: u64,
    fids: HashMap<Fid, FidState>,
    cleanup: VecDeque<CleanupAction>,
    cleanup_started: bool,
    terminal_reason: Option<CloseReason>,
    terminal_effect_delivered: bool,
}

impl Session {
    /// Constructs an independently owned session without performing external I/O.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidConfig`] when resource limits are inconsistent.
    pub fn new(config: SessionConfig, context: SessionContext) -> Result<Self, SessionError> {
        config.limits.validate()?;
        Ok(Self {
            decoder: FrameDecoder::new(&config.limits),
            config,
            context,
            negotiated_msize: None,
            effects: VecDeque::new(),
            accounting: Accounting::default(),
            operation_ids: OperationIdAllocator::new(),
            pending_by_tag: HashMap::new(),
            flushes: HashMap::new(),
            tag_by_operation: HashMap::new(),
            abandoned_operations: HashMap::new(),
            last_issued_operation_id: 0,
            fids: HashMap::new(),
            cleanup: VecDeque::new(),
            cleanup_started: false,
            terminal_reason: None,
            terminal_effect_delivered: false,
        })
    }

    /// Returns immutable host-supplied connection context.
    pub const fn context(&self) -> &SessionContext {
        &self.context
    }

    /// Returns the negotiated `msize`, or `None` before a supported version handshake.
    pub const fn negotiated_msize(&self) -> Option<u32> {
        self.negotiated_msize
    }

    /// Reports whether the session accepts input, is closing, or is fully drained.
    pub fn status(&self) -> SessionStatus {
        if self.terminal_reason.is_none() {
            SessionStatus::Active
        } else if self.effects.is_empty()
            && self.pending_by_tag.is_empty()
            && self.flushes.is_empty()
            && self.abandoned_operations.is_empty()
            && self.cleanup.is_empty()
            && self.terminal_effect_delivered
        {
            SessionStatus::Drained
        } else {
            SessionStatus::Closing
        }
    }

    /// Reports whether no input tail, emitted effect, or active operation is currently retained.
    pub fn is_quiescent(&self) -> bool {
        self.decoder.retained_len() == 0
            && self.effects.is_empty()
            && self.pending_by_tag.is_empty()
            && self.flushes.is_empty()
            && self.abandoned_operations.is_empty()
            && self.cleanup.is_empty()
    }

    /// Returns the number of client tags currently awaiting external terminal work.
    pub fn in_flight_requests(&self) -> usize {
        self.pending_by_tag.len() + self.flushes.len()
    }

    /// Returns the number of installed or transition-reserved fids.
    pub fn fid_count(&self) -> usize {
        self.fids.len()
    }

    /// Begins explicit host shutdown and schedules cancellation/release for all live state.
    ///
    /// This method is idempotent. The host continues polling effects and supplying terminal
    /// completions until [`SessionStatus::Drained`] is reported. Dropping earlier deliberately
    /// abandons any backend state whose cleanup was not completed.
    pub fn begin_close(&mut self) {
        if self.terminal_reason.is_none() {
            self.terminal_reason = Some(CloseReason::HostShutdown);
        }
        self.start_cleanup();
    }

    /// Records transport closure and begins the same explicit cleanup as [`Self::begin_close`].
    pub fn transport_closed(&mut self) {
        self.begin_close();
    }

    /// Supplies an arbitrary fragment/coalescing of stream transport bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed session error for malformed input, closure, or backpressure. Malformed
    /// framing also schedules a terminal [`Effect::CloseSession`].
    pub fn receive_bytes(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.ensure_active()?;
        let frames = match self.decoder.push(bytes, self.active_msize()) {
            Ok(frames) => frames,
            Err(error) => {
                self.fail_decode(error.clone());
                return Err(error.into());
            }
        };
        for frame in frames {
            self.receive_decoded_frame(frame)?;
            if self.terminal_reason.is_some() {
                break;
            }
        }
        Ok(())
    }

    /// Supplies one owned complete message-transport frame.
    ///
    /// # Errors
    ///
    /// Returns a typed session error when the frame is malformed, the session is closing, or an
    /// effect cannot fit configured queue bounds.
    pub fn receive_frame(&mut self, frame: Vec<u8>) -> Result<(), SessionError> {
        self.ensure_active()?;
        self.receive_decoded_frame(frame)
    }

    /// Supplies one terminal result for a previously emitted external operation.
    ///
    /// # Errors
    ///
    /// Returns [`CompletionError`] for unknown, duplicate/stale, or wrong-kind completions. A
    /// rejected completion leaves every pending request unchanged.
    pub fn complete(&mut self, completion: Completion) -> Result<(), CompletionError> {
        let operation_id = completion.operation_id();
        if let Some(tag) = self.tag_by_operation.get(&operation_id).copied() {
            let pending = self
                .pending_by_tag
                .get(&tag)
                .expect("operation index and tag index remain synchronized");
            pending.expected.validate(&completion)?;

            self.tag_by_operation.remove(&operation_id);
            let pending = self
                .pending_by_tag
                .remove(&tag)
                .expect("validated pending entry still exists");
            Accounting::release(&mut self.accounting.in_flight_tags, 1);
            Accounting::release(
                &mut self.accounting.pending_write_bytes,
                pending.write_bytes,
            );
            self.finish_pending(tag, pending, completion);
            return Ok(());
        }

        if let Some(expected) = self.abandoned_operations.get(&operation_id).copied() {
            expected.validate(&completion)?;
            self.abandoned_operations.remove(&operation_id);
            return Ok(());
        }

        if operation_id.get() <= self.last_issued_operation_id {
            Err(CompletionError::DuplicateOperation(operation_id))
        } else {
            Err(CompletionError::UnknownOperation(operation_id))
        }
    }

    /// Polls the next owned effect in deterministic per-session order.
    pub fn poll_effect(&mut self) -> Option<Effect> {
        if let Some(effect) = self.effects.pop_front() {
            Accounting::release(&mut self.accounting.queued_effects, 1);
            Accounting::release(
                &mut self.accounting.queued_effect_bytes,
                effect.retained_bytes(),
            );
            return Some(effect);
        }
        if let Some(action) = self.cleanup.pop_front() {
            return self.cleanup_effect(action);
        }
        if !self.terminal_effect_delivered
            && let Some(reason) = self.terminal_reason.clone()
        {
            self.terminal_effect_delivered = true;
            return Some(Effect::CloseSession { reason });
        }
        None
    }

    fn cleanup_effect(&mut self, action: CleanupAction) -> Option<Effect> {
        match action {
            CleanupAction::Cancel { operation_id, kind } => {
                Some(Effect::Cancel { operation_id, kind })
            }
            CleanupAction::Policy(request) => {
                let expected = ExpectedCompletion::Policy(request.expected_result());
                let operation_id = match self.operation_ids.allocate() {
                    Ok(operation_id) => operation_id,
                    Err(_) => {
                        self.terminal_reason = Some(CloseReason::ResourceExhausted);
                        return None;
                    }
                };
                self.last_issued_operation_id = operation_id.get();
                self.abandoned_operations.insert(operation_id, expected);
                Some(Effect::Policy {
                    operation_id,
                    request,
                })
            }
            CleanupAction::Filesystem(request) => {
                let expected = ExpectedCompletion::Filesystem(request.expected_result());
                let operation_id = match self.operation_ids.allocate() {
                    Ok(operation_id) => operation_id,
                    Err(_) => {
                        self.terminal_reason = Some(CloseReason::ResourceExhausted);
                        return None;
                    }
                };
                self.last_issued_operation_id = operation_id.get();
                self.abandoned_operations.insert(operation_id, expected);
                Some(Effect::Filesystem {
                    operation_id,
                    request,
                })
            }
        }
    }

    fn receive_decoded_frame(&mut self, frame: Vec<u8>) -> Result<(), SessionError> {
        let request = match decode_request(&frame, &self.config.limits, self.active_msize()) {
            Ok(request) => request,
            Err(error) => {
                self.fail_decode(error.clone());
                return Err(error.into());
            }
        };
        self.dispatch(request)
    }

    fn dispatch(&mut self, request: Request) -> Result<(), SessionError> {
        if request.body.message_type() != crate::protocol::MessageType::Tversion
            && request.tag == Tag::NOTAG
        {
            self.fail_protocol("NOTAG is reserved for Tversion");
            return Ok(());
        }
        if request.body.message_type() != crate::protocol::MessageType::Tversion
            && (self.pending_by_tag.contains_key(&request.tag)
                || self.flushes.contains_key(&request.tag))
        {
            return self.queue_state_error(request.tag, SessionStateError::DuplicateTag);
        }
        match request.body {
            RequestBody::Version { msize, version } => {
                self.handle_version(request.tag, msize, version)
            }
            _ if self.negotiated_msize.is_none() => {
                self.queue_state_error(request.tag, SessionStateError::NotNegotiated)
            }
            RequestBody::Auth {
                afid,
                uname,
                aname,
                n_uname,
            } => self.handle_auth(request.tag, afid, uname, aname, n_uname),
            RequestBody::Attach {
                fid,
                afid,
                uname,
                aname,
                n_uname,
            } => self.handle_attach(request.tag, fid, afid, uname, aname, n_uname),
            RequestBody::Flush { old_tag } => self.handle_flush(request.tag, old_tag),
            RequestBody::Read { fid, offset, count }
                if matches!(self.fids.get(&fid), Some(FidState::Auth { .. })) =>
            {
                self.handle_auth_read(request.tag, fid, offset, count)
            }
            RequestBody::Write { fid, offset, data }
                if matches!(self.fids.get(&fid), Some(FidState::Auth { .. })) =>
            {
                self.handle_auth_write(request.tag, fid, offset, data)
            }
            RequestBody::Clunk { fid }
                if matches!(self.fids.get(&fid), Some(FidState::Auth { .. })) =>
            {
                self.handle_auth_clunk(request.tag, fid)
            }
            RequestBody::Lopen { fid, flags } => self.handle_open(request.tag, fid, flags),
            RequestBody::Lcreate {
                fid,
                name,
                flags,
                mode,
                gid,
            } => self.handle_create(request.tag, fid, name, flags, mode, gid),
            RequestBody::Statfs { fid } => self.handle_statfs(request.tag, fid),
            RequestBody::Getattr { fid, mask } => self.handle_getattr(request.tag, fid, mask),
            RequestBody::Setattr { fid, attributes } => {
                self.handle_setattr(request.tag, fid, attributes)
            }
            RequestBody::Readlink { fid } => self.handle_readlink(request.tag, fid),
            RequestBody::Readdir { fid, offset, count } => {
                self.handle_readdir(request.tag, fid, offset, count)
            }
            RequestBody::Mkdir {
                directory,
                name,
                mode,
                gid,
            } => self.handle_mkdir(request.tag, directory, name, mode, gid),
            RequestBody::Mknod {
                directory,
                name,
                mode,
                major,
                minor,
                gid,
            } => self.handle_mknod(request.tag, directory, name, mode, major, minor, gid),
            RequestBody::Symlink {
                directory,
                name,
                target,
                gid,
            } => self.handle_symlink(request.tag, directory, name, target, gid),
            RequestBody::Link {
                directory,
                target,
                name,
            } => self.handle_link(request.tag, directory, target, name),
            RequestBody::Rename {
                fid,
                directory,
                name,
            } => self.handle_rename(request.tag, fid, directory, name),
            RequestBody::RenameAt {
                old_directory,
                old_name,
                new_directory,
                new_name,
            } => self.handle_rename_at(
                request.tag,
                old_directory,
                old_name,
                new_directory,
                new_name,
            ),
            RequestBody::UnlinkAt {
                directory,
                name,
                flags,
            } => self.handle_unlink_at(request.tag, directory, name, flags),
            RequestBody::XattrWalk { fid, new_fid, name } => {
                self.handle_xattr_walk(request.tag, fid, new_fid, name)
            }
            RequestBody::XattrCreate {
                fid,
                name,
                size,
                flags,
            } => self.handle_xattr_create(request.tag, fid, name, size, flags),
            RequestBody::Fsync { fid, data_only } => self.handle_fsync(request.tag, fid, data_only),
            RequestBody::Lock { fid, lock } => self.handle_lock(request.tag, fid, lock),
            RequestBody::Getlock { fid, lock } => self.handle_getlock(request.tag, fid, lock),
            RequestBody::Read { fid, offset, count } => {
                self.handle_file_read(request.tag, fid, offset, count)
            }
            RequestBody::Write { fid, offset, data } => {
                self.handle_file_write(request.tag, fid, offset, data)
            }
            RequestBody::Walk {
                fid,
                new_fid,
                names,
            } => self.handle_walk(request.tag, fid, new_fid, names),
            RequestBody::Clunk { fid } => self.handle_object_clunk(request.tag, fid),
            RequestBody::Remove { fid } => self.handle_remove(request.tag, fid),
        }
    }

    fn handle_flush(&mut self, tag: Tag, old_tag: Tag) -> Result<(), SessionError> {
        if let Some(target) = self.pending_by_tag.get(&old_tag) {
            let operation_id = target.operation_id;
            let kind = target.kind;
            let needs_cancel = !target.cancellation_requested;
            Accounting::reserve(
                &mut self.accounting.in_flight_tags,
                1,
                self.config.limits.max_in_flight_tags,
                LimitKind::InFlightTags,
            )?;
            if needs_cancel {
                if let Err(error) = self.queue_effect(Effect::Cancel { operation_id, kind }) {
                    Accounting::release(&mut self.accounting.in_flight_tags, 1);
                    return Err(error);
                }
            }
            let target = self
                .pending_by_tag
                .get_mut(&old_tag)
                .expect("flush target remains active while cancellation is queued");
            target.response_suppressed = true;
            target.cancellation_requested = true;
            target.flush_waiters.push(tag);
            self.flushes.insert(tag, FlushState::default());
            return Ok(());
        }

        if self.flushes.contains_key(&old_tag) {
            Accounting::reserve(
                &mut self.accounting.in_flight_tags,
                1,
                self.config.limits.max_in_flight_tags,
                LimitKind::InFlightTags,
            )?;
            self.flushes.insert(tag, FlushState::default());
            let target = self
                .flushes
                .get_mut(&old_tag)
                .expect("nested flush target remains active");
            target.response_suppressed = true;
            target.waiters.push(tag);
            return Ok(());
        }

        self.queue_response(Response {
            tag,
            body: ResponseBody::Flush,
        })
    }

    fn handle_fsync(&mut self, tag: Tag, fid: Fid, data_only: bool) -> Result<(), SessionError> {
        let object = match self.object_fid(tag, fid)? {
            Some(object) => object,
            None => return Ok(()),
        };
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Fsync { open, data_only },
            Continuation::Fsync,
            0,
        )
    }

    fn handle_lock(
        &mut self,
        tag: Tag,
        fid: Fid,
        lock: crate::protocol::LockRequest,
    ) -> Result<(), SessionError> {
        let object = match self.object_fid(tag, fid)? {
            Some(object) => object,
            None => return Ok(()),
        };
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Lock { open, lock },
            Continuation::Lock,
            0,
        )
    }

    fn handle_getlock(
        &mut self,
        tag: Tag,
        fid: Fid,
        lock: crate::protocol::Lock,
    ) -> Result<(), SessionError> {
        let object = match self.object_fid(tag, fid)? {
            Some(object) => object,
            None => return Ok(()),
        };
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Getlock { open, lock },
            Continuation::Getlock,
            0,
        )
    }

    fn handle_xattr_walk(
        &mut self,
        tag: Tag,
        fid: Fid,
        new_fid: Fid,
        name: String,
    ) -> Result<(), SessionError> {
        if fid == new_fid {
            return self.queue_state_error(tag, SessionStateError::InvalidRequest);
        }
        let source = match self.object_fid(tag, fid)? {
            Some(source) => source,
            None => return Ok(()),
        };
        let operation = FilesystemOperation::XattrWalk {
            object: source.object,
            name,
        };
        if source.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        if !self.claim_fid(tag, new_fid)? {
            return Ok(());
        }
        let continuation = Continuation::XattrWalk {
            new_fid,
            source: source.clone(),
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(source.context.clone(), operation),
            continuation,
            0,
        ) {
            self.retire_fid(new_fid);
            return Err(error);
        }
        Ok(())
    }

    fn handle_xattr_create(
        &mut self,
        tag: Tag,
        fid: Fid,
        name: String,
        size: u64,
        flags: crate::protocol::XattrFlags,
    ) -> Result<(), SessionError> {
        let original = match self.object_fid(tag, fid)? {
            Some(original) => original,
            None => return Ok(()),
        };
        if original.open.is_some() {
            return self.queue_state_error(tag, SessionStateError::FidAlreadyOpen);
        }
        let operation = FilesystemOperation::XattrCreate {
            object: original.object,
            name,
            size,
            flags,
        };
        if original.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.fids.insert(fid, FidState::Pending);
        let continuation = Continuation::XattrCreate {
            fid,
            original: original.clone(),
            expected_size: size,
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(original.context.clone(), operation),
            continuation,
            0,
        ) {
            self.fids.insert(fid, FidState::Object(original));
            return Err(error);
        }
        Ok(())
    }

    fn handle_readdir(
        &mut self,
        tag: Tag,
        fid: Fid,
        offset: u64,
        count: u32,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        if !object.qid.ty.contains(crate::protocol::QidType::DIRECTORY) || object.xattr.is_some() {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::ENOTDIR)),
            });
        }
        let maximum = count.min(self.active_msize().saturating_sub(11));
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::ReadDir {
                open,
                offset,
                count: maximum,
            },
            Continuation::Readdir { maximum },
            0,
        )
    }

    fn handle_mkdir(
        &mut self,
        tag: Tag,
        directory: Fid,
        name: String,
        mode: u32,
        gid: u32,
    ) -> Result<(), SessionError> {
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        self.emit_filesystem_checked(
            tag,
            parent.context,
            parent.capabilities,
            FilesystemOperation::Mkdir {
                directory: parent.object,
                name,
                mode,
                gid,
            },
            Continuation::Mkdir,
            0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_mknod(
        &mut self,
        tag: Tag,
        directory: Fid,
        name: String,
        mode: u32,
        major: u32,
        minor: u32,
        gid: u32,
    ) -> Result<(), SessionError> {
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        self.emit_filesystem_checked(
            tag,
            parent.context,
            parent.capabilities,
            FilesystemOperation::Mknod {
                directory: parent.object,
                name,
                mode,
                major,
                minor,
                gid,
            },
            Continuation::Mknod,
            0,
        )
    }

    fn handle_symlink(
        &mut self,
        tag: Tag,
        directory: Fid,
        name: String,
        target: String,
        gid: u32,
    ) -> Result<(), SessionError> {
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        self.emit_filesystem_checked(
            tag,
            parent.context,
            parent.capabilities,
            FilesystemOperation::Symlink {
                directory: parent.object,
                name,
                target,
                gid,
            },
            Continuation::Symlink,
            0,
        )
    }

    fn handle_link(
        &mut self,
        tag: Tag,
        directory: Fid,
        target: Fid,
        name: String,
    ) -> Result<(), SessionError> {
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        let target = match self.object_fid(tag, target)? {
            Some(target) => target,
            None => return Ok(()),
        };
        if parent.context != target.context {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EXDEV)),
            });
        }
        self.emit_filesystem_checked(
            tag,
            parent.context,
            parent.capabilities,
            FilesystemOperation::Link {
                directory: parent.object,
                target: target.object,
                name,
            },
            Continuation::NoState(ResponseBody::Link),
            0,
        )
    }

    fn handle_rename(
        &mut self,
        tag: Tag,
        fid: Fid,
        directory: Fid,
        name: String,
    ) -> Result<(), SessionError> {
        let object = match self.object_fid(tag, fid)? {
            Some(object) => object,
            None => return Ok(()),
        };
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        if object.context != parent.context {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EXDEV)),
            });
        }
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Rename {
                object: object.object,
                directory: parent.object,
                name,
            },
            Continuation::NoState(ResponseBody::Rename),
            0,
        )
    }

    fn handle_rename_at(
        &mut self,
        tag: Tag,
        old_directory: Fid,
        old_name: String,
        new_directory: Fid,
        new_name: String,
    ) -> Result<(), SessionError> {
        let old_parent = match self.directory_fid(tag, old_directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        let new_parent = match self.directory_fid(tag, new_directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        if old_parent.context != new_parent.context {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EXDEV)),
            });
        }
        self.emit_filesystem_checked(
            tag,
            old_parent.context,
            old_parent.capabilities,
            FilesystemOperation::RenameAt {
                old_directory: old_parent.object,
                old_name,
                new_directory: new_parent.object,
                new_name,
            },
            Continuation::NoState(ResponseBody::RenameAt),
            0,
        )
    }

    fn handle_unlink_at(
        &mut self,
        tag: Tag,
        directory: Fid,
        name: String,
        flags: crate::protocol::UnlinkFlags,
    ) -> Result<(), SessionError> {
        let parent = match self.directory_fid(tag, directory)? {
            Some(parent) => parent,
            None => return Ok(()),
        };
        self.emit_filesystem_checked(
            tag,
            parent.context,
            parent.capabilities,
            FilesystemOperation::UnlinkAt {
                directory: parent.object,
                name,
                flags,
            },
            Continuation::NoState(ResponseBody::UnlinkAt),
            0,
        )
    }

    fn handle_statfs(&mut self, tag: Tag, fid: Fid) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Statfs {
                object: object.object,
            },
            Continuation::Statfs,
            0,
        )
    }

    fn handle_getattr(
        &mut self,
        tag: Tag,
        fid: Fid,
        mask: crate::protocol::GetattrMask,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Getattr {
                object: object.object,
                mask,
            },
            Continuation::Getattr,
            0,
        )
    }

    fn handle_setattr(
        &mut self,
        tag: Tag,
        fid: Fid,
        attributes: crate::protocol::SetAttributes,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Setattr {
                object: object.object,
                attributes,
            },
            Continuation::Setattr,
            0,
        )
    }

    fn handle_readlink(&mut self, tag: Tag, fid: Fid) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if object.xattr.is_some() {
            return self.queue_state_error(tag, SessionStateError::WrongFidKind);
        }
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::Readlink {
                object: object.object,
            },
            Continuation::Readlink,
            0,
        )
    }

    fn handle_open(
        &mut self,
        tag: Tag,
        fid: Fid,
        flags: crate::protocol::OpenFlags,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if object.open.is_some() || object.xattr.is_some() {
            return self.queue_state_error(tag, SessionStateError::FidAlreadyOpen);
        }
        let operation = FilesystemOperation::Open {
            object: object.object,
            flags,
        };
        if object.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.fids.insert(fid, FidState::Pending);
        let continuation = Continuation::Open {
            fid,
            original: object.clone(),
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(object.context.clone(), operation),
            continuation,
            0,
        ) {
            self.fids.insert(fid, FidState::Object(object));
            return Err(error);
        }
        Ok(())
    }

    fn handle_create(
        &mut self,
        tag: Tag,
        fid: Fid,
        name: String,
        flags: crate::protocol::OpenFlags,
        mode: u32,
        gid: u32,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if object.open.is_some() || object.xattr.is_some() {
            return self.queue_state_error(tag, SessionStateError::FidAlreadyOpen);
        }
        let operation = FilesystemOperation::Create {
            directory: object.object,
            name,
            flags,
            mode,
            gid,
        };
        if object.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.fids.insert(fid, FidState::Pending);
        let continuation = Continuation::Create {
            fid,
            original: object.clone(),
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(object.context.clone(), operation),
            continuation,
            0,
        ) {
            self.fids.insert(fid, FidState::Object(object));
            return Err(error);
        }
        Ok(())
    }

    fn handle_file_read(
        &mut self,
        tag: Tag,
        fid: Fid,
        offset: u64,
        count: u32,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if let Some(xattr) = object.xattr {
            return self.handle_xattr_read(tag, object, xattr, offset, count);
        }
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        let maximum = count.min(self.active_msize().saturating_sub(11));
        if offset.checked_add(u64::from(maximum)).is_none() {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EOVERFLOW)),
            });
        }
        let operation = FilesystemOperation::Read {
            open,
            offset,
            count: maximum,
        };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            operation,
            Continuation::Read { maximum },
            0,
        )
    }

    fn handle_xattr_read(
        &mut self,
        tag: Tag,
        object: ObjectFid,
        xattr: XattrState,
        offset: u64,
        count: u32,
    ) -> Result<(), SessionError> {
        let XattrMode::Read { size } = xattr.mode else {
            return self.queue_state_error(tag, SessionStateError::WrongFidKind);
        };
        if offset >= size {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Read { data: Vec::new() },
            });
        }
        let remaining = size - offset;
        let maximum = count
            .min(self.active_msize().saturating_sub(11))
            .min(u32::try_from(remaining).unwrap_or(u32::MAX));
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            FilesystemOperation::XattrRead {
                xattr: xattr.handle,
                offset,
                count: maximum,
            },
            Continuation::XattrRead { maximum },
            0,
        )
    }

    fn handle_xattr_write(
        &mut self,
        tag: Tag,
        fid: Fid,
        object: ObjectFid,
        xattr: XattrState,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<(), SessionError> {
        let XattrMode::Write {
            expected_size,
            written,
        } = xattr.mode
        else {
            return self.queue_state_error(tag, SessionStateError::WrongFidKind);
        };
        let Some(end) = offset.checked_add(data.len() as u64) else {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EOVERFLOW)),
            });
        };
        if offset != written || end > expected_size {
            return self.queue_state_error(tag, SessionStateError::InvalidRequest);
        }
        let requested = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let write_bytes = data.len();
        let operation = FilesystemOperation::XattrWrite {
            xattr: xattr.handle,
            offset,
            data,
        };
        if object.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.fids.insert(fid, FidState::Pending);
        let continuation = Continuation::XattrWrite {
            fid,
            original: object.clone(),
            requested,
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(object.context.clone(), operation),
            continuation,
            write_bytes,
        ) {
            self.fids.insert(fid, FidState::Object(object));
            return Err(error);
        }
        Ok(())
    }

    fn handle_file_write(
        &mut self,
        tag: Tag,
        fid: Fid,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if let Some(xattr) = object.xattr {
            return self.handle_xattr_write(tag, fid, object, xattr, offset, data);
        }
        let Some(open) = object.open else {
            return self.queue_state_error(tag, SessionStateError::FidNotOpen);
        };
        if offset.checked_add(data.len() as u64).is_none() {
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::EOVERFLOW)),
            });
        }
        let requested = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let write_bytes = data.len();
        let operation = FilesystemOperation::Write { open, offset, data };
        self.emit_filesystem_checked(
            tag,
            object.context,
            object.capabilities,
            operation,
            Continuation::Write { requested },
            write_bytes,
        )
    }

    fn handle_walk(
        &mut self,
        tag: Tag,
        source_fid: Fid,
        new_fid: Fid,
        names: Vec<String>,
    ) -> Result<(), SessionError> {
        let source = match self.fids.get(&source_fid) {
            Some(FidState::Object(source)) => source.clone(),
            Some(FidState::Auth { .. } | FidState::Pending) => {
                return self.queue_state_error(tag, SessionStateError::WrongFidKind);
            }
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if source.open.is_some() || source.xattr.is_some() {
            return self.queue_state_error(tag, SessionStateError::FidAlreadyOpen);
        }

        if names.is_empty() {
            if new_fid != source_fid {
                if !self.claim_fid(tag, new_fid)? {
                    return Ok(());
                }
                self.fids.insert(new_fid, FidState::Object(source));
            }
            return self.queue_response(Response {
                tag,
                body: ResponseBody::Walk { qids: Vec::new() },
            });
        }

        let requested = names.len();
        let operation = FilesystemOperation::Walk {
            start: source.object,
            names,
        };
        if source.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        if new_fid == source_fid {
            self.fids.insert(source_fid, FidState::Pending);
        } else if !self.claim_fid(tag, new_fid)? {
            return Ok(());
        }
        let continuation = Continuation::Walk {
            source_fid,
            new_fid,
            source: source.clone(),
            requested,
        };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(source.context.clone(), operation),
            continuation.clone(),
            0,
        ) {
            self.rollback_continuation(&continuation);
            return Err(error);
        }
        Ok(())
    }

    fn handle_object_clunk(&mut self, tag: Tag, fid: Fid) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(FidState::Pending | FidState::Auth { .. }) => {
                return self.queue_state_error(tag, SessionStateError::WrongFidKind);
            }
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if let Some(XattrState {
            handle,
            mode:
                XattrMode::Write {
                    expected_size,
                    written,
                },
        }) = object.xattr
        {
            let (operation, continuation) = if written == expected_size {
                (
                    FilesystemOperation::XattrCommit {
                        xattr: handle,
                        expected_size,
                    },
                    Continuation::XattrCommitFid { fid },
                )
            } else {
                (
                    FilesystemOperation::Release {
                        object: object.object,
                        open: object.open,
                        xattr: Some(handle),
                    },
                    Continuation::XattrAbortFid {
                        fid,
                        errno: LinuxErrno::EINVAL,
                    },
                )
            };
            if object.capabilities.require_for(&operation).is_err() {
                return self.queue_state_error(tag, SessionStateError::Unsupported);
            }
            self.fids.insert(fid, FidState::Pending);
            if let Err(error) = self.emit_filesystem(
                tag,
                FilesystemRequest::new(object.context.clone(), operation),
                continuation,
                0,
            ) {
                self.fids.insert(fid, FidState::Object(object));
                return Err(error);
            }
            return Ok(());
        }
        self.fids.insert(fid, FidState::Pending);
        let operation = FilesystemOperation::Release {
            object: object.object,
            open: object.open,
            xattr: object.xattr.map(|state| state.handle),
        };
        let continuation = Continuation::ReleaseFid { fid };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(object.context.clone(), operation),
            continuation,
            0,
        ) {
            self.fids.insert(fid, FidState::Object(object));
            return Err(error);
        }
        Ok(())
    }

    fn handle_remove(&mut self, tag: Tag, fid: Fid) -> Result<(), SessionError> {
        let object = match self.fids.get(&fid) {
            Some(FidState::Object(object)) => object.clone(),
            Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
            None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        if object.xattr.is_some() {
            return self.queue_state_error(tag, SessionStateError::WrongFidKind);
        }
        let operation = FilesystemOperation::Remove {
            object: object.object,
            open: object.open,
            xattr: object.xattr.map(|state| state.handle),
        };
        if object.capabilities.require_for(&operation).is_err() {
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.fids.insert(fid, FidState::Pending);
        let continuation = Continuation::RemoveFid { fid };
        if let Err(error) = self.emit_filesystem(
            tag,
            FilesystemRequest::new(object.context.clone(), operation),
            continuation,
            0,
        ) {
            self.fids.insert(fid, FidState::Object(object));
            return Err(error);
        }
        Ok(())
    }

    fn handle_auth(
        &mut self,
        tag: Tag,
        afid: Fid,
        uname: String,
        aname: String,
        n_uname: u32,
    ) -> Result<(), SessionError> {
        if afid == Fid::NOFID {
            return self.queue_state_error(tag, SessionStateError::InvalidRequest);
        }
        if !self.claim_fid(tag, afid)? {
            return Ok(());
        }
        let request = PolicyRequest::StartAuth {
            afid,
            identity: UserIdentity::from_wire(uname, n_uname),
            export_name: aname,
            transport_principal: self.context.transport_principal.clone(),
        };
        if let Err(error) = self.emit_policy(tag, request, Continuation::AuthStart { fid: afid }, 0)
        {
            self.retire_fid(afid);
            return Err(error);
        }
        Ok(())
    }

    fn handle_attach(
        &mut self,
        tag: Tag,
        fid: Fid,
        afid: Fid,
        uname: String,
        aname: String,
        n_uname: u32,
    ) -> Result<(), SessionError> {
        if fid == Fid::NOFID {
            return self.queue_state_error(tag, SessionStateError::InvalidRequest);
        }
        let auth = if afid == Fid::NOFID {
            None
        } else {
            match self.fids.get(&afid) {
                Some(FidState::Auth { handle, .. }) => Some(*handle),
                Some(_) => return self.queue_state_error(tag, SessionStateError::WrongFidKind),
                None => return self.queue_state_error(tag, SessionStateError::UnknownFid),
            }
        };
        if !self.claim_fid(tag, fid)? {
            return Ok(());
        }
        let request = PolicyRequest::Attach {
            fid,
            auth,
            identity: UserIdentity::from_wire(uname, n_uname),
            export_name: aname,
            transport_principal: self.context.transport_principal.clone(),
        };
        if let Err(error) = self.emit_policy(tag, request, Continuation::Attach { fid }, 0) {
            self.retire_fid(fid);
            return Err(error);
        }
        Ok(())
    }

    fn handle_auth_read(
        &mut self,
        tag: Tag,
        fid: Fid,
        offset: u64,
        count: u32,
    ) -> Result<(), SessionError> {
        let handle = match self.fids.get(&fid) {
            Some(FidState::Auth { handle, .. }) => *handle,
            _ => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        let maximum = count.min(self.active_msize().saturating_sub(11));
        self.emit_policy(
            tag,
            PolicyRequest::ReadAuth {
                handle,
                offset,
                count: maximum,
            },
            Continuation::AuthRead { maximum },
            0,
        )
    }

    fn handle_auth_write(
        &mut self,
        tag: Tag,
        fid: Fid,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<(), SessionError> {
        let handle = match self.fids.get(&fid) {
            Some(FidState::Auth { handle, .. }) => *handle,
            _ => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        let requested = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let write_bytes = data.len();
        self.emit_policy(
            tag,
            PolicyRequest::WriteAuth {
                handle,
                offset,
                data,
            },
            Continuation::AuthWrite { requested },
            write_bytes,
        )
    }

    fn handle_auth_clunk(&mut self, tag: Tag, fid: Fid) -> Result<(), SessionError> {
        let (qid, handle) = match self.fids.get(&fid) {
            Some(FidState::Auth { qid, handle }) => (*qid, *handle),
            _ => return self.queue_state_error(tag, SessionStateError::UnknownFid),
        };
        self.fids.insert(fid, FidState::Pending);
        if let Err(error) = self.emit_policy(
            tag,
            PolicyRequest::ClunkAuth { handle },
            Continuation::AuthClunk { fid },
            0,
        ) {
            self.fids.insert(fid, FidState::Auth { qid, handle });
            return Err(error);
        }
        Ok(())
    }

    fn handle_version(
        &mut self,
        tag: Tag,
        requested_msize: u32,
        version: String,
    ) -> Result<(), SessionError> {
        if tag != Tag::NOTAG {
            return self.queue_state_error(tag, SessionStateError::InvalidTag);
        }

        // The smallest supported Rversion is the 7-byte header, msize, two-byte string length,
        // and the eight-byte dialect. A smaller request cannot be answered within its own bound.
        const MINIMUM_NEGOTIATED_MSIZE: u32 = 21;
        if requested_msize < MINIMUM_NEGOTIATED_MSIZE {
            self.fail_protocol("Tversion msize cannot contain Rversion");
            return Ok(());
        }

        self.reset_for_version();
        let effective = requested_msize.min(self.config.limits.max_frame_size);
        let accepted = version == VERSION_9P2000_L;
        self.negotiated_msize = accepted.then_some(effective);
        self.queue_response(Response {
            tag,
            body: ResponseBody::Version {
                msize: effective,
                version: if accepted {
                    VERSION_9P2000_L.into()
                } else {
                    "unknown".into()
                },
            },
        })
    }

    fn reset_for_version(&mut self) {
        self.negotiated_msize = None;
        self.decoder.clear();
        let flush_count = self.flushes.len();
        self.flushes.clear();
        Accounting::release(&mut self.accounting.in_flight_tags, flush_count);
        let pending = core::mem::take(&mut self.pending_by_tag);
        self.tag_by_operation.clear();
        for (_, request) in pending {
            Accounting::release(&mut self.accounting.in_flight_tags, 1);
            Accounting::release(
                &mut self.accounting.pending_write_bytes,
                request.write_bytes,
            );
            self.abandoned_operations
                .insert(request.operation_id, request.expected);
            self.cleanup.push_back(CleanupAction::Cancel {
                operation_id: request.operation_id,
                kind: request.kind,
            });
        }
        self.drain_fids_to_cleanup();
    }

    fn queue_state_error(
        &mut self,
        tag: Tag,
        error: SessionStateError,
    ) -> Result<(), SessionError> {
        self.queue_response(Response {
            tag,
            body: ResponseBody::Lerror(LinuxWireError(error.errno())),
        })
    }

    fn queue_response(&mut self, response: Response) -> Result<(), SessionError> {
        let bytes = encode_response(&response, self.active_msize())?;
        self.queue_effect(Effect::SendFrame { bytes })
    }

    fn queue_effect(&mut self, effect: Effect) -> Result<(), SessionError> {
        Accounting::reserve(
            &mut self.accounting.queued_effects,
            1,
            self.config.limits.max_queued_effects,
            LimitKind::QueuedEffects,
        )?;
        if let Err(error) = Accounting::reserve(
            &mut self.accounting.queued_effect_bytes,
            effect.retained_bytes(),
            self.config.limits.max_queued_effect_bytes,
            LimitKind::QueuedEffectBytes,
        ) {
            Accounting::release(&mut self.accounting.queued_effects, 1);
            return Err(error.into());
        }
        self.effects.push_back(effect);
        Ok(())
    }

    fn object_fid(&mut self, tag: Tag, fid: Fid) -> Result<Option<ObjectFid>, SessionError> {
        match self.fids.get(&fid) {
            Some(FidState::Object(object)) if object.xattr.is_none() => Ok(Some(object.clone())),
            Some(_) => {
                self.queue_state_error(tag, SessionStateError::WrongFidKind)?;
                Ok(None)
            }
            None => {
                self.queue_state_error(tag, SessionStateError::UnknownFid)?;
                Ok(None)
            }
        }
    }

    fn directory_fid(&mut self, tag: Tag, fid: Fid) -> Result<Option<ObjectFid>, SessionError> {
        let Some(object) = self.object_fid(tag, fid)? else {
            return Ok(None);
        };
        if !object.qid.ty.contains(crate::protocol::QidType::DIRECTORY) {
            self.queue_response(Response {
                tag,
                body: ResponseBody::Lerror(LinuxWireError(LinuxErrno::ENOTDIR)),
            })?;
            return Ok(None);
        }
        Ok(Some(object))
    }

    fn claim_fid(&mut self, tag: Tag, fid: Fid) -> Result<bool, SessionError> {
        if self.fids.contains_key(&fid) {
            self.queue_state_error(tag, SessionStateError::DuplicateFid)?;
            return Ok(false);
        }
        Accounting::reserve(
            &mut self.accounting.fids,
            1,
            self.config.limits.max_fids,
            LimitKind::Fids,
        )?;
        self.fids.insert(fid, FidState::Pending);
        Ok(true)
    }

    fn retire_fid(&mut self, fid: Fid) {
        if self.fids.remove(&fid).is_some() {
            Accounting::release(&mut self.accounting.fids, 1);
        }
    }

    fn start_cleanup(&mut self) {
        if self.cleanup_started {
            return;
        }
        self.cleanup_started = true;
        self.decoder.clear();
        for pending in self.pending_by_tag.values_mut() {
            pending.response_suppressed = true;
            if !pending.cancellation_requested {
                pending.cancellation_requested = true;
                self.cleanup.push_back(CleanupAction::Cancel {
                    operation_id: pending.operation_id,
                    kind: pending.kind,
                });
            }
        }
        for flush in self.flushes.values_mut() {
            flush.response_suppressed = true;
        }
        self.drain_fids_to_cleanup();
    }

    fn drain_fids_to_cleanup(&mut self) {
        let fids = core::mem::take(&mut self.fids);
        for (_, fid) in fids {
            Accounting::release(&mut self.accounting.fids, 1);
            match fid {
                FidState::Auth { handle, .. } => self
                    .cleanup
                    .push_back(CleanupAction::Policy(PolicyRequest::ClunkAuth { handle })),
                FidState::Object(object) => {
                    let operation = FilesystemOperation::Release {
                        object: object.object,
                        open: object.open,
                        xattr: object.xattr.map(|state| state.handle),
                    };
                    self.cleanup
                        .push_back(CleanupAction::Filesystem(FilesystemRequest::new(
                            object.context,
                            operation,
                        )));
                }
                FidState::Pending => {}
            }
        }
    }

    fn emit_filesystem(
        &mut self,
        tag: Tag,
        request: FilesystemRequest,
        continuation: Continuation,
        write_bytes: usize,
    ) -> Result<(), SessionError> {
        let expected = ExpectedCompletion::Filesystem(request.expected_result());
        self.emit_operation(
            tag,
            expected,
            CancelKind::Filesystem,
            continuation,
            write_bytes,
            |operation_id| Effect::Filesystem {
                operation_id,
                request,
            },
        )
    }

    fn emit_filesystem_checked(
        &mut self,
        tag: Tag,
        context: RequestContext,
        capabilities: CapabilitySet,
        operation: FilesystemOperation,
        continuation: Continuation,
        write_bytes: usize,
    ) -> Result<(), SessionError> {
        if capabilities.require_for(&operation).is_err() {
            self.rollback_continuation(&continuation);
            return self.queue_state_error(tag, SessionStateError::Unsupported);
        }
        self.emit_filesystem(
            tag,
            FilesystemRequest::new(context, operation),
            continuation,
            write_bytes,
        )
    }

    fn emit_policy(
        &mut self,
        tag: Tag,
        request: PolicyRequest,
        continuation: Continuation,
        write_bytes: usize,
    ) -> Result<(), SessionError> {
        let expected = ExpectedCompletion::Policy(request.expected_result());
        self.emit_operation(
            tag,
            expected,
            CancelKind::Policy,
            continuation,
            write_bytes,
            |operation_id| Effect::Policy {
                operation_id,
                request,
            },
        )
    }

    fn emit_operation<F>(
        &mut self,
        tag: Tag,
        expected: ExpectedCompletion,
        kind: CancelKind,
        continuation: Continuation,
        write_bytes: usize,
        make_effect: F,
    ) -> Result<(), SessionError>
    where
        F: FnOnce(OperationId) -> Effect,
    {
        if self.pending_by_tag.contains_key(&tag) {
            return self.queue_state_error(tag, SessionStateError::DuplicateTag);
        }
        Accounting::reserve(
            &mut self.accounting.in_flight_tags,
            1,
            self.config.limits.max_in_flight_tags,
            LimitKind::InFlightTags,
        )?;
        if let Err(error) = Accounting::reserve(
            &mut self.accounting.pending_write_bytes,
            write_bytes,
            self.config.limits.max_pending_write_bytes,
            LimitKind::PendingWriteBytes,
        ) {
            Accounting::release(&mut self.accounting.in_flight_tags, 1);
            return Err(error.into());
        }
        let operation_id = match self.operation_ids.allocate() {
            Ok(operation_id) => operation_id,
            Err(error) => {
                Accounting::release(&mut self.accounting.in_flight_tags, 1);
                Accounting::release(&mut self.accounting.pending_write_bytes, write_bytes);
                return Err(error);
            }
        };
        let effect = make_effect(operation_id);
        if let Err(error) = self.queue_effect(effect) {
            Accounting::release(&mut self.accounting.in_flight_tags, 1);
            Accounting::release(&mut self.accounting.pending_write_bytes, write_bytes);
            // Allocated IDs are intentionally burned even when the effect could not be queued.
            self.last_issued_operation_id = operation_id.get();
            return Err(error);
        }
        self.last_issued_operation_id = operation_id.get();
        self.pending_by_tag.insert(
            tag,
            PendingOperation {
                operation_id,
                expected,
                kind,
                continuation,
                response_suppressed: false,
                cancellation_requested: false,
                flush_waiters: Vec::new(),
                write_bytes,
            },
        );
        self.tag_by_operation.insert(operation_id, tag);
        Ok(())
    }

    fn finish_pending(&mut self, tag: Tag, pending: PendingOperation, completion: Completion) {
        let response = match completion {
            Completion::Filesystem {
                result: Err(error), ..
            } => {
                self.rollback_continuation(&pending.continuation);
                ResponseBody::Lerror(LinuxWireError(error.errno))
            }
            Completion::Policy {
                result: Err(error), ..
            } => {
                self.rollback_continuation(&pending.continuation);
                ResponseBody::Lerror(LinuxWireError(error.errno))
            }
            Completion::Cancelled { .. } => {
                self.rollback_continuation(&pending.continuation);
                ResponseBody::Lerror(LinuxWireError(LinuxErrno::ECANCELED))
            }
            Completion::Policy {
                result: Ok(result), ..
            } => self.finish_policy_success(pending.continuation.clone(), result),
            Completion::Filesystem {
                result: Ok(result), ..
            } => self.finish_filesystem_success(pending.continuation.clone(), result),
        };
        if self.terminal_reason.is_some() {
            self.drain_fids_to_cleanup();
        }
        if !pending.response_suppressed
            && self
                .queue_response(Response {
                    tag,
                    body: response,
                })
                .is_err()
        {
            self.fail_protocol("terminal completion response could not be queued");
        }
        for flush_tag in pending.flush_waiters {
            self.finish_flush(flush_tag);
        }
    }

    fn finish_flush(&mut self, tag: Tag) {
        let Some(flush) = self.flushes.remove(&tag) else {
            return;
        };
        Accounting::release(&mut self.accounting.in_flight_tags, 1);
        if !flush.response_suppressed
            && self
                .queue_response(Response {
                    tag,
                    body: ResponseBody::Flush,
                })
                .is_err()
        {
            self.fail_protocol("Rflush could not be queued");
        }
        for waiter in flush.waiters {
            self.finish_flush(waiter);
        }
    }

    fn finish_policy_success(
        &mut self,
        continuation: Continuation,
        result: PolicyResult,
    ) -> ResponseBody {
        match (continuation, result) {
            (Continuation::AuthStart { fid }, PolicyResult::AuthStarted { qid, handle }) => {
                self.fids.insert(fid, FidState::Auth { qid, handle });
                ResponseBody::Auth { qid }
            }
            (Continuation::Attach { fid }, PolicyResult::Attached(attach)) => {
                let AttachResult {
                    principal,
                    export,
                    root,
                    qid,
                    capabilities,
                } = attach;
                self.fids.insert(
                    fid,
                    FidState::Object(ObjectFid {
                        object: root,
                        qid,
                        context: RequestContext::new(self.context.session_id, principal, export),
                        capabilities,
                        open: None,
                        xattr: None,
                    }),
                );
                ResponseBody::Attach { qid }
            }
            (Continuation::AuthRead { maximum }, PolicyResult::AuthRead { data }) => {
                if data.len() <= maximum as usize {
                    ResponseBody::Read { data }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::AuthWrite { requested }, PolicyResult::AuthWritten { count }) => {
                if count <= requested {
                    ResponseBody::Write { count }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::AuthClunk { fid }, PolicyResult::AuthClunked) => {
                self.retire_fid(fid);
                ResponseBody::Clunk
            }
            (Continuation::NoState(response), _) => response,
            (continuation, _) => {
                self.rollback_continuation(&continuation);
                ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
            }
        }
    }

    fn finish_filesystem_success(
        &mut self,
        continuation: Continuation,
        result: FilesystemResult,
    ) -> ResponseBody {
        match (continuation, result) {
            (
                Continuation::Walk {
                    source_fid,
                    new_fid,
                    mut source,
                    requested,
                },
                FilesystemResult::Walked(WalkResult { elements }),
            ) => {
                if elements.is_empty() || elements.len() > requested {
                    self.rollback_walk(source_fid, new_fid, source);
                    let errno = if elements.is_empty() {
                        LinuxErrno::ENOENT
                    } else {
                        LinuxErrno::EIO
                    };
                    return ResponseBody::Lerror(LinuxWireError(errno));
                }
                let last = elements.last().expect("non-empty checked walk result");
                source.object = last.object;
                source.qid = last.qid;
                source.open = None;
                source.xattr = None;
                self.fids.insert(new_fid, FidState::Object(source));
                ResponseBody::Walk {
                    qids: elements.into_iter().map(|element| element.qid).collect(),
                }
            }
            (Continuation::ReleaseFid { fid }, FilesystemResult::Released) => {
                self.retire_fid(fid);
                ResponseBody::Clunk
            }
            (Continuation::RemoveFid { fid }, FilesystemResult::Removed) => {
                self.retire_fid(fid);
                ResponseBody::Remove
            }
            (Continuation::Open { fid, mut original }, FilesystemResult::Opened(opened)) => {
                original.qid = opened.qid;
                original.open = Some(opened.open);
                original.xattr = None;
                self.fids.insert(fid, FidState::Object(original));
                ResponseBody::Lopen {
                    qid: opened.qid,
                    io_unit: opened.io_unit.min(
                        self.active_msize()
                            .saturating_sub(u32::try_from(IO_HEADER_SIZE).unwrap_or(u32::MAX)),
                    ),
                }
            }
            (Continuation::Create { fid, mut original }, FilesystemResult::Created(created)) => {
                original.object = created.object;
                original.qid = created.qid;
                original.open = Some(created.open);
                original.xattr = None;
                self.fids.insert(fid, FidState::Object(original));
                ResponseBody::Lcreate {
                    qid: created.qid,
                    io_unit: created.io_unit.min(
                        self.active_msize()
                            .saturating_sub(u32::try_from(IO_HEADER_SIZE).unwrap_or(u32::MAX)),
                    ),
                }
            }
            (Continuation::Read { maximum }, FilesystemResult::Read(data)) => {
                if data.len() <= maximum as usize {
                    ResponseBody::Read { data }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::Write { requested }, FilesystemResult::Written(count)) => {
                if count <= requested {
                    ResponseBody::Write { count }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::Statfs, FilesystemResult::Statfs(statfs)) => {
                ResponseBody::Statfs(statfs)
            }
            (Continuation::Getattr, FilesystemResult::Attributes(attributes)) => {
                if attributes.validate().is_ok() {
                    ResponseBody::Getattr(attributes)
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::Setattr, FilesystemResult::AttributesSet) => ResponseBody::Setattr,
            (Continuation::Readlink, FilesystemResult::LinkTarget(target)) => {
                let encoded_size = 9usize.saturating_add(target.len());
                if target.len() <= self.config.limits.max_string_bytes
                    && encoded_size <= self.active_msize() as usize
                {
                    ResponseBody::Readlink { target }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (Continuation::Readdir { maximum }, FilesystemResult::DirectoryRead(entries)) => {
                match encode_directory_entries(
                    &entries,
                    maximum as usize,
                    self.config.limits.max_string_bytes,
                ) {
                    Ok(data) => ResponseBody::Readdir { data },
                    Err(_) => ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO)),
                }
            }
            (Continuation::Mkdir, FilesystemResult::DirectoryCreated(node)) => {
                ResponseBody::Mkdir { qid: node.qid }
            }
            (Continuation::Mknod, FilesystemResult::NodeCreated(node)) => {
                ResponseBody::Mknod { qid: node.qid }
            }
            (Continuation::Symlink, FilesystemResult::SymlinkCreated(node)) => {
                ResponseBody::Symlink { qid: node.qid }
            }
            (
                Continuation::XattrWalk {
                    new_fid,
                    mut source,
                },
                FilesystemResult::XattrWalked(walked),
            ) => {
                source.open = None;
                source.xattr = Some(XattrState {
                    handle: walked.xattr,
                    mode: XattrMode::Read { size: walked.size },
                });
                self.fids.insert(new_fid, FidState::Object(source));
                ResponseBody::XattrWalk { size: walked.size }
            }
            (
                Continuation::XattrCreate {
                    fid,
                    mut original,
                    expected_size,
                },
                FilesystemResult::XattrCreated(created),
            ) => {
                original.open = None;
                original.xattr = Some(XattrState {
                    handle: created.xattr,
                    mode: XattrMode::Write {
                        expected_size,
                        written: 0,
                    },
                });
                self.fids.insert(fid, FidState::Object(original));
                ResponseBody::XattrCreate
            }
            (Continuation::XattrRead { maximum }, FilesystemResult::XattrRead(data)) => {
                if data.len() <= maximum as usize {
                    ResponseBody::Read { data }
                } else {
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                }
            }
            (
                Continuation::XattrWrite {
                    fid,
                    mut original,
                    requested,
                },
                FilesystemResult::XattrWritten(count),
            ) => {
                if count > requested {
                    self.fids.insert(fid, FidState::Object(original));
                    ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
                } else {
                    if let Some(XattrState {
                        mode: XattrMode::Write { written, .. },
                        ..
                    }) = &mut original.xattr
                    {
                        *written = written.saturating_add(u64::from(count));
                    }
                    self.fids.insert(fid, FidState::Object(original));
                    ResponseBody::Write { count }
                }
            }
            (Continuation::XattrCommitFid { fid }, FilesystemResult::XattrCommitted) => {
                self.retire_fid(fid);
                ResponseBody::Clunk
            }
            (Continuation::XattrAbortFid { fid, errno }, FilesystemResult::Released) => {
                self.retire_fid(fid);
                ResponseBody::Lerror(LinuxWireError(errno))
            }
            (Continuation::Fsync, FilesystemResult::Synced) => ResponseBody::Fsync,
            (Continuation::Lock, FilesystemResult::Locked(status)) => ResponseBody::Lock { status },
            (Continuation::Getlock, FilesystemResult::LockQueried(lock)) => {
                ResponseBody::Getlock { lock }
            }
            (Continuation::NoState(response), _) => response,
            (continuation, _) => {
                self.rollback_continuation(&continuation);
                ResponseBody::Lerror(LinuxWireError(LinuxErrno::EIO))
            }
        }
    }

    fn rollback_walk(&mut self, source_fid: Fid, new_fid: Fid, source: ObjectFid) {
        if source_fid == new_fid {
            self.fids.insert(source_fid, FidState::Object(source));
        } else {
            self.retire_fid(new_fid);
        }
    }

    fn rollback_continuation(&mut self, continuation: &Continuation) {
        match continuation {
            Continuation::AuthStart { fid }
            | Continuation::Attach { fid }
            | Continuation::AuthClunk { fid } => self.retire_fid(*fid),
            Continuation::Walk {
                source_fid,
                new_fid,
                source,
                ..
            } => self.rollback_walk(*source_fid, *new_fid, source.clone()),
            Continuation::ReleaseFid { fid } | Continuation::RemoveFid { fid } => {
                self.retire_fid(*fid);
            }
            Continuation::Open { fid, original } | Continuation::Create { fid, original } => {
                self.fids.insert(*fid, FidState::Object(original.clone()));
            }
            Continuation::NoState(_)
            | Continuation::AuthRead { .. }
            | Continuation::AuthWrite { .. }
            | Continuation::Read { .. }
            | Continuation::Write { .. }
            | Continuation::Statfs
            | Continuation::Getattr
            | Continuation::Setattr
            | Continuation::Readlink
            | Continuation::Readdir { .. }
            | Continuation::Mkdir
            | Continuation::Mknod
            | Continuation::Symlink
            | Continuation::XattrRead { .. }
            | Continuation::Fsync
            | Continuation::Lock
            | Continuation::Getlock => {}
            Continuation::XattrWalk { new_fid, .. } => self.retire_fid(*new_fid),
            Continuation::XattrCreate { fid, original, .. }
            | Continuation::XattrWrite { fid, original, .. } => {
                self.fids.insert(*fid, FidState::Object(original.clone()));
            }
            Continuation::XattrCommitFid { fid } | Continuation::XattrAbortFid { fid, .. } => {
                self.retire_fid(*fid);
            }
        }
    }

    const fn active_msize(&self) -> u32 {
        match self.negotiated_msize {
            Some(msize) => msize,
            None => self.config.limits.max_frame_size,
        }
    }

    fn ensure_active(&self) -> Result<(), SessionError> {
        if self.terminal_reason.is_some() {
            Err(SessionError::Closing)
        } else {
            Ok(())
        }
    }

    fn fail_decode(&mut self, error: DecodeError) {
        self.decoder.clear();
        self.terminal_reason = Some(CloseReason::MalformedInput(error));
        self.start_cleanup();
    }

    fn fail_protocol(&mut self, reason: &'static str) {
        self.decoder.clear();
        self.terminal_reason = Some(CloseReason::ProtocolViolation(reason));
        self.start_cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PolicyError,
        filesystem::{
            CapabilitySet, ExportId, FilesystemError, ObjectHandle, OpenHandle, OpenResult,
            PrincipalId, WalkElement, WalkResult, XattrCreateResult, XattrHandle,
        },
        protocol::{Fid, OpenFlags, Qid, QidType, UserIdentity, XattrFlags},
    };

    fn active_session() -> Session {
        let mut session = Session::new(
            SessionConfig::default(),
            SessionContext::new(crate::SessionId::new(1)),
        )
        .unwrap();
        session.negotiated_msize = Some(4096);
        session
    }

    fn session_with_limits(limits: crate::Limits) -> Session {
        let mut session = Session::new(
            SessionConfig::new(limits).unwrap(),
            SessionContext::new(crate::SessionId::new(1)),
        )
        .unwrap();
        session.negotiated_msize = Some(4096);
        session
    }

    fn policy_request(fid: u32) -> PolicyRequest {
        PolicyRequest::StartAuth {
            afid: Fid::new(fid),
            identity: UserIdentity::from_wire("u".into(), 1),
            export_name: "/".into(),
            transport_principal: None,
        }
    }

    fn install_root(session: &mut Session, fid: Fid) {
        session.fids.insert(
            fid,
            FidState::Object(ObjectFid {
                object: ObjectHandle::new(1),
                qid: Qid::new(QidType::DIRECTORY, 0, 1),
                context: RequestContext::new(
                    session.context.session_id,
                    PrincipalId::new("p"),
                    ExportId::new("e"),
                ),
                capabilities: CapabilitySet::ALL,
                open: None,
                xattr: None,
            }),
        );
        session.accounting.fids += 1;
    }

    #[test]
    fn wrong_kind_completion_does_not_mutate_pending_request() {
        let mut session = active_session();
        session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!("expected policy effect")
        };
        let error = session
            .complete(Completion::Filesystem {
                operation_id,
                result: Err(crate::FilesystemError::new(LinuxErrno::EIO)),
            })
            .unwrap_err();
        assert!(matches!(error, CompletionError::WrongKind { .. }));
        assert_eq!(session.in_flight_requests(), 1);

        session
            .complete(Completion::Policy {
                operation_id,
                result: Err(PolicyError::new(LinuxErrno::EACCES)),
            })
            .unwrap();
        assert_eq!(session.in_flight_requests(), 0);
        assert_eq!(
            session.complete(Completion::Policy {
                operation_id,
                result: Err(PolicyError::new(LinuxErrno::EACCES)),
            }),
            Err(CompletionError::DuplicateOperation(operation_id))
        );
    }

    #[test]
    fn replies_follow_completion_order_and_keep_original_tags() {
        let mut session = active_session();
        for tag in [1u16, 2] {
            session
                .emit_policy(
                    Tag::new(tag),
                    policy_request(u32::from(tag)),
                    Continuation::NoState(ResponseBody::Clunk),
                    0,
                )
                .unwrap();
        }
        let Effect::Policy {
            operation_id: first,
            ..
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };
        let Effect::Policy {
            operation_id: second,
            ..
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };
        for operation_id in [second, first] {
            session
                .complete(Completion::Policy {
                    operation_id,
                    result: Err(PolicyError::new(LinuxErrno::EACCES)),
                })
                .unwrap();
        }
        let tags: Vec<u16> = (0..2)
            .map(|_| match session.poll_effect().unwrap() {
                Effect::SendFrame { bytes } => u16::from_le_bytes([bytes[5], bytes[6]]),
                _ => panic!(),
            })
            .collect();
        assert_eq!(tags, [2, 1]);
    }

    #[test]
    fn partial_walk_installs_destination_and_returns_resolved_prefix() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .handle_walk(
                Tag::new(1),
                Fid::new(1),
                Fid::new(2),
                vec!["a".into(), "missing".into()],
            )
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::Walked(WalkResult {
                    elements: vec![WalkElement {
                        object: ObjectHandle::new(2),
                        qid: Qid::new(QidType::DIRECTORY, 0, 2),
                    }],
                })),
            })
            .unwrap();
        assert!(matches!(
            session.fids.get(&Fid::new(2)),
            Some(FidState::Object(_))
        ));
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 111);
        assert_eq!(&bytes[7..9], &[1, 0]);
    }

    #[test]
    fn clunk_failure_still_retires_fid() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .handle_object_clunk(Tag::new(1), Fid::new(1))
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Err(FilesystemError::new(LinuxErrno::EIO)),
            })
            .unwrap();
        assert!(!session.fids.contains_key(&Fid::new(1)));
    }

    #[test]
    fn begin_close_cancels_pending_work_suppresses_reply_and_drains() {
        let mut session = active_session();
        session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };

        session.begin_close();
        assert_eq!(session.status(), SessionStatus::Closing);
        assert!(matches!(
            session.receive_frame(vec![7, 0, 0, 0, 108, 1, 0]),
            Err(SessionError::Closing)
        ));
        assert_eq!(
            session.poll_effect(),
            Some(Effect::Cancel {
                operation_id,
                kind: CancelKind::Policy
            })
        );
        assert!(matches!(
            session.poll_effect(),
            Some(Effect::CloseSession { .. })
        ));
        session
            .complete(Completion::Policy {
                operation_id,
                result: Err(PolicyError::new(LinuxErrno::EINTR)),
            })
            .unwrap();
        assert_eq!(session.poll_effect(), None);
        assert_eq!(session.status(), SessionStatus::Drained);
    }

    #[test]
    fn close_turns_live_fid_into_tracked_release_completion() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session.begin_close();
        let Effect::Filesystem {
            operation_id,
            request,
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            request.operation,
            FilesystemOperation::Release { .. }
        ));
        assert!(matches!(
            session.poll_effect(),
            Some(Effect::CloseSession { .. })
        ));
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::Released),
            })
            .unwrap();
        assert_eq!(session.status(), SessionStatus::Drained);
    }

    #[test]
    fn open_installs_handle_and_read_rejects_oversized_backend_result() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .handle_open(Tag::new(1), Fid::new(1), OpenFlags::RDONLY)
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        let qid = Qid::new(QidType::FILE, 0, 1);
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::Opened(OpenResult {
                    qid,
                    open: OpenHandle::new(8),
                    io_unit: u32::MAX,
                })),
            })
            .unwrap();
        let _ = session.poll_effect();

        session
            .handle_file_read(Tag::new(2), Fid::new(1), 0, 2)
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::Read(vec![1, 2, 3])),
            })
            .unwrap();
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 7);
        assert_eq!(
            u32::from_le_bytes(bytes[7..11].try_into().unwrap()),
            LinuxErrno::EIO.get()
        );
    }

    #[test]
    fn xattr_write_commits_on_clunk_only_after_exact_size() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .handle_xattr_create(
                Tag::new(1),
                Fid::new(1),
                "user.test".into(),
                3,
                XattrFlags::EMPTY,
            )
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::XattrCreated(XattrCreateResult {
                    xattr: XattrHandle::new(3),
                })),
            })
            .unwrap();
        let _ = session.poll_effect();

        session
            .handle_file_write(Tag::new(2), Fid::new(1), 0, vec![1, 2, 3])
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::XattrWritten(3)),
            })
            .unwrap();
        let _ = session.poll_effect();

        session
            .handle_object_clunk(Tag::new(3), Fid::new(1))
            .unwrap();
        let Effect::Filesystem {
            operation_id,
            request,
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            request.operation,
            FilesystemOperation::XattrCommit {
                expected_size: 3,
                ..
            }
        ));
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::XattrCommitted),
            })
            .unwrap();
        assert_eq!(session.fid_count(), 0);
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 121);
    }

    #[test]
    fn flush_waits_for_terminal_work_and_suppresses_old_reply() {
        let mut session = active_session();
        session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session.handle_flush(Tag::new(2), Tag::new(1)).unwrap();
        assert_eq!(
            session.poll_effect(),
            Some(Effect::Cancel {
                operation_id,
                kind: CancelKind::Policy,
            })
        );
        assert_eq!(session.poll_effect(), None);

        session
            .complete(Completion::Policy {
                operation_id,
                result: Err(PolicyError::new(LinuxErrno::EIO)),
            })
            .unwrap();
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 109);
        assert_eq!(u16::from_le_bytes([bytes[5], bytes[6]]), 2);
        assert_eq!(session.poll_effect(), None);
    }

    #[test]
    fn absent_flush_target_replies_immediately() {
        let mut session = active_session();
        session.handle_flush(Tag::new(2), Tag::new(99)).unwrap();
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 109);
        assert_eq!(u16::from_le_bytes([bytes[5], bytes[6]]), 2);
    }

    #[test]
    fn multiple_and_nested_flushes_have_one_deterministic_terminal_reply() {
        let mut session = active_session();
        session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let Effect::Policy { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session.handle_flush(Tag::new(2), Tag::new(1)).unwrap();
        let _ = session.poll_effect();
        session.handle_flush(Tag::new(3), Tag::new(1)).unwrap();
        session.handle_flush(Tag::new(4), Tag::new(2)).unwrap();
        assert_eq!(session.poll_effect(), None);

        session
            .complete(Completion::Cancelled {
                operation_id,
                kind: CancelKind::Policy,
            })
            .unwrap();
        let tags: Vec<u16> = core::iter::from_fn(|| session.poll_effect())
            .map(|effect| match effect {
                Effect::SendFrame { bytes } => {
                    assert_eq!(bytes[4], 109);
                    u16::from_le_bytes([bytes[5], bytes[6]])
                }
                _ => panic!(),
            })
            .collect();
        assert_eq!(tags, [4, 3]);
        assert_eq!(session.in_flight_requests(), 0);
        assert_eq!(
            session.complete(Completion::Cancelled {
                operation_id,
                kind: CancelKind::Policy,
            }),
            Err(CompletionError::DuplicateOperation(operation_id))
        );
    }

    #[test]
    fn committed_result_advances_state_but_flush_suppresses_its_reply() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .handle_open(Tag::new(1), Fid::new(1), OpenFlags::RDONLY)
            .unwrap();
        let Effect::Filesystem { operation_id, .. } = session.poll_effect().unwrap() else {
            panic!()
        };
        session.handle_flush(Tag::new(2), Tag::new(1)).unwrap();
        let _ = session.poll_effect();
        session
            .complete(Completion::Filesystem {
                operation_id,
                result: Ok(FilesystemResult::Opened(OpenResult {
                    qid: Qid::new(QidType::FILE, 0, 1),
                    open: OpenHandle::new(9),
                    io_unit: 0,
                })),
            })
            .unwrap();
        let Some(FidState::Object(object)) = session.fids.get(&Fid::new(1)) else {
            panic!()
        };
        assert_eq!(object.open, Some(OpenHandle::new(9)));
        let Effect::SendFrame { bytes } = session.poll_effect().unwrap() else {
            panic!()
        };
        assert_eq!(bytes[4], 109);
        assert_eq!(session.poll_effect(), None);
    }

    #[test]
    fn delayed_completions_bound_tags_flushes_fids_and_write_bytes() {
        let limits = crate::Limits {
            max_in_flight_tags: 1,
            max_fids: 1,
            max_pending_write_bytes: 2,
            ..crate::Limits::default()
        };
        let mut session = session_with_limits(limits);

        session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let _ = session.poll_effect();
        assert!(matches!(
            session.handle_flush(Tag::new(2), Tag::new(1)),
            Err(SessionError::Limit(crate::LimitExceeded {
                kind: LimitKind::InFlightTags,
                ..
            }))
        ));
        assert!(!session.flushes.contains_key(&Tag::new(2)));

        assert!(session.claim_fid(Tag::new(3), Fid::new(1)).unwrap());
        assert!(matches!(
            session.claim_fid(Tag::new(4), Fid::new(2)),
            Err(SessionError::Limit(crate::LimitExceeded {
                kind: LimitKind::Fids,
                ..
            }))
        ));

        let write_limits = crate::Limits {
            max_pending_write_bytes: 2,
            ..crate::Limits::default()
        };
        let mut write_session = session_with_limits(write_limits);
        assert!(matches!(
            write_session.emit_policy(
                Tag::new(5),
                PolicyRequest::WriteAuth {
                    handle: crate::AuthHandle::new(1),
                    offset: 0,
                    data: vec![1, 2, 3],
                },
                Continuation::AuthWrite { requested: 3 },
                3,
            ),
            Err(SessionError::Limit(crate::LimitExceeded {
                kind: LimitKind::PendingWriteBytes,
                ..
            }))
        ));

        let queue_limits = crate::Limits {
            max_queued_effects: 1,
            ..crate::Limits::default()
        };
        let mut queue_session = session_with_limits(queue_limits);
        queue_session
            .emit_policy(
                Tag::new(1),
                policy_request(1),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        assert!(matches!(
            queue_session.emit_policy(
                Tag::new(2),
                policy_request(2),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            ),
            Err(SessionError::Limit(crate::LimitExceeded {
                kind: LimitKind::QueuedEffects,
                ..
            }))
        ));
    }

    #[test]
    fn renegotiation_resets_protocol_state_but_tracks_old_cleanup() {
        let mut session = active_session();
        install_root(&mut session, Fid::new(1));
        session
            .emit_policy(
                Tag::new(1),
                policy_request(2),
                Continuation::NoState(ResponseBody::Clunk),
                0,
            )
            .unwrap();
        let Effect::Policy {
            operation_id: old_operation,
            ..
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };

        session
            .handle_version(Tag::NOTAG, 4096, VERSION_9P2000_L.into())
            .unwrap();
        assert_eq!(session.in_flight_requests(), 0);
        assert_eq!(session.fid_count(), 0);
        assert!(matches!(
            session.poll_effect(),
            Some(Effect::SendFrame { .. })
        ));
        assert_eq!(
            session.poll_effect(),
            Some(Effect::Cancel {
                operation_id: old_operation,
                kind: CancelKind::Policy,
            })
        );
        let Effect::Filesystem {
            operation_id: release_operation,
            ..
        } = session.poll_effect().unwrap()
        else {
            panic!()
        };
        session
            .complete(Completion::Policy {
                operation_id: old_operation,
                result: Err(PolicyError::new(LinuxErrno::EINTR)),
            })
            .unwrap();
        session
            .complete(Completion::Filesystem {
                operation_id: release_operation,
                result: Ok(FilesystemResult::Released),
            })
            .unwrap();
        assert_eq!(session.poll_effect(), None);
        assert_eq!(session.negotiated_msize(), Some(4096));
    }

    #[test]
    fn randomized_action_completion_traces_preserve_indexes_and_accounting() {
        fn random(state: &mut u64) -> u64 {
            *state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            *state
        }

        for seed in 0..64u64 {
            let mut state = seed;
            let mut session = active_session();
            let mut next_tag = 1u16;
            for _ in 0..256 {
                match random(&mut state) % 4 {
                    0 if next_tag < u16::MAX - 1 => {
                        let tag = Tag::new(next_tag);
                        next_tag += 1;
                        let _ = session.emit_policy(
                            tag,
                            policy_request(u32::from(next_tag)),
                            Continuation::NoState(ResponseBody::Clunk),
                            0,
                        );
                    }
                    1 if next_tag < u16::MAX - 1 => {
                        let target = session
                            .pending_by_tag
                            .keys()
                            .chain(session.flushes.keys())
                            .min_by_key(|tag| tag.get())
                            .copied();
                        if let Some(target) = target {
                            let tag = Tag::new(next_tag);
                            next_tag += 1;
                            let _ = session.handle_flush(tag, target);
                        }
                    }
                    2 => {
                        let operation_id = session
                            .tag_by_operation
                            .keys()
                            .min_by_key(|operation_id| operation_id.get())
                            .copied();
                        if let Some(operation_id) = operation_id {
                            let _ = session.complete(Completion::Policy {
                                operation_id,
                                result: Err(PolicyError::new(LinuxErrno::EIO)),
                            });
                        }
                    }
                    _ => {
                        let _ = session.poll_effect();
                    }
                }

                assert_eq!(session.tag_by_operation.len(), session.pending_by_tag.len());
                for (operation_id, tag) in &session.tag_by_operation {
                    assert_eq!(
                        session
                            .pending_by_tag
                            .get(tag)
                            .map(|pending| pending.operation_id),
                        Some(*operation_id)
                    );
                }
                assert_eq!(
                    session.accounting.in_flight_tags,
                    session.pending_by_tag.len() + session.flushes.len()
                );
                assert_eq!(session.accounting.fids, session.fids.len());
                assert_eq!(session.accounting.queued_effects, session.effects.len());
            }

            let active: Vec<_> = session.tag_by_operation.keys().copied().collect();
            for operation_id in active {
                session
                    .complete(Completion::Policy {
                        operation_id,
                        result: Err(PolicyError::new(LinuxErrno::EIO)),
                    })
                    .unwrap();
            }
            assert!(session.pending_by_tag.is_empty());
            assert!(session.flushes.is_empty());
        }
    }
}
