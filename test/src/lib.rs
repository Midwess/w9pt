//! Minimal application-owned 9P-to-S3 forwarding servers used by integration tests.
//!
//! This is intentionally not a production filesystem. It demonstrates the
//! embedding boundary: [`w9pt::Session`] owns 9P protocol state, while this
//! application receives TCP stream bytes or complete binary WebSocket messages,
//! performs SeaweedFS S3 operations, and returns completions.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, error::Error, io, sync::Arc};

use aws_sdk_s3::{
    Client, Config,
    config::{BehaviorVersion, Credentials, Region},
    primitives::ByteStream,
    types::ChecksumAlgorithm,
};
use axum::{
    Router,
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, get},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use w9pt::{
    Completion, Effect, PolicyError, PolicyRequest, PolicyResult, Session, SessionConfig,
    SessionContext, SessionId,
    filesystem::{
        AttachResult, Capability, CapabilitySet, CreateResult, ExportId, FilesystemError,
        FilesystemOperation, FilesystemRequest, FilesystemResult, LinuxErrno, ObjectHandle,
        OpenHandle, OpenResult, PrincipalId, WalkElement, WalkResult,
    },
    protocol::{OpenFlags, Qid, QidType},
};

/// Error type used by the demonstration server boundary.
pub type BoxError = Box<dyn Error + Send + Sync>;

const ROOT_OBJECT: u128 = 1;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// WebSocket subprotocol required by the HTTP 9P endpoint.
pub const WEBSOCKET_9P_SUBPROTOCOL: &str = "9p";
/// Maximum complete 9P frame accepted in one WebSocket message.
pub const WEBSOCKET_9P_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct FileEntry {
    key: String,
    version: u32,
}

#[derive(Clone, Copy, Debug)]
struct OpenEntry {
    object: u128,
    readable: bool,
    writable: bool,
}

#[derive(Debug)]
struct Namespace {
    next_object: u128,
    next_open: u128,
    names: BTreeMap<String, u128>,
    files: BTreeMap<u128, FileEntry>,
    opens: BTreeMap<u128, OpenEntry>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self {
            next_object: ROOT_OBJECT + 1,
            next_open: 1,
            names: BTreeMap::new(),
            files: BTreeMap::new(),
            opens: BTreeMap::new(),
        }
    }
}

/// Minimal application-side filesystem that persists regular-file bytes in S3.
#[derive(Clone, Debug)]
pub struct AppFilesystem {
    client: Client,
    bucket: Arc<str>,
    prefix: Arc<str>,
    namespace: Arc<Mutex<Namespace>>,
}

impl AppFilesystem {
    /// Creates an empty application namespace over a caller-created S3 client.
    pub fn new(client: Client, bucket: impl Into<String>, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into().trim_matches('/').to_owned();
        Self {
            client,
            bucket: bucket.into().into(),
            prefix: prefix.into(),
            namespace: Arc::new(Mutex::new(Namespace::default())),
        }
    }

    /// Returns the S3 key currently assigned to one root-level test file.
    pub async fn file_key(&self, name: &str) -> Option<String> {
        let namespace = self.namespace.lock().await;
        let object = namespace.names.get(name)?;
        namespace.files.get(object).map(|file| file.key.clone())
    }

    /// Handles one policy effect. The test application supports unauthenticated attach only.
    pub fn handle_policy(&self, request: PolicyRequest) -> Result<PolicyResult, PolicyError> {
        match request {
            PolicyRequest::Attach {
                identity,
                export_name,
                ..
            } => {
                let principal = if identity.name.is_empty() {
                    identity
                        .numeric_id
                        .map_or_else(|| "anonymous".to_owned(), |id| id.to_string())
                } else {
                    identity.name
                };
                let export = if export_name.is_empty() {
                    "seaweedfs-test".to_owned()
                } else {
                    export_name
                };
                Ok(PolicyResult::Attached(AttachResult {
                    principal: PrincipalId::new(principal),
                    export: ExportId::new(export),
                    root: ObjectHandle::new(ROOT_OBJECT),
                    qid: root_qid(),
                    capabilities: application_capabilities(),
                }))
            }
            PolicyRequest::StartAuth { .. }
            | PolicyRequest::ReadAuth { .. }
            | PolicyRequest::WriteAuth { .. }
            | PolicyRequest::ClunkAuth { .. } => Err(PolicyError::new(LinuxErrno::EOPNOTSUPP)),
        }
    }

    /// Handles one filesystem effect and returns a client-safe completion result.
    pub async fn handle_filesystem(
        &self,
        request: FilesystemRequest,
    ) -> Result<FilesystemResult, FilesystemError> {
        match self.execute(request.operation).await {
            Ok(result) => Ok(result),
            Err(AppFailure::Client(errno)) => Err(FilesystemError::new(errno)),
            Err(AppFailure::Internal(message)) => {
                eprintln!("test application filesystem failure: {message}");
                Err(FilesystemError::new(LinuxErrno::EIO))
            }
        }
    }

    async fn execute(
        &self,
        operation: FilesystemOperation,
    ) -> Result<FilesystemResult, AppFailure> {
        match operation {
            FilesystemOperation::Walk { start, names } => self.walk(start, names).await,
            FilesystemOperation::Open { object, flags } => self.open(object, flags).await,
            FilesystemOperation::Create {
                directory,
                name,
                flags,
                ..
            } => self.create(directory, name, flags).await,
            FilesystemOperation::Read {
                open,
                offset,
                count,
            } => self.read(open, offset, count).await,
            FilesystemOperation::Write { open, offset, data } => {
                self.write(open, offset, data).await
            }
            FilesystemOperation::Release { open, .. } => {
                if let Some(open) = open {
                    self.namespace.lock().await.opens.remove(&open.get());
                }
                Ok(FilesystemResult::Released)
            }
            _ => Err(AppFailure::Client(LinuxErrno::EOPNOTSUPP)),
        }
    }

    async fn walk(
        &self,
        start: ObjectHandle,
        names: Vec<String>,
    ) -> Result<FilesystemResult, AppFailure> {
        let namespace = self.namespace.lock().await;
        let mut current = start.get();
        let mut elements = Vec::with_capacity(names.len());
        for name in names {
            if current != ROOT_OBJECT {
                if elements.is_empty() {
                    return Err(AppFailure::Client(LinuxErrno::ENOTDIR));
                }
                break;
            }
            let object = match name.as_str() {
                "." | ".." => ROOT_OBJECT,
                _ => match namespace.names.get(&name) {
                    Some(object) => *object,
                    None if elements.is_empty() => {
                        return Err(AppFailure::Client(LinuxErrno::ENOENT));
                    }
                    None => break,
                },
            };
            let qid = if object == ROOT_OBJECT {
                root_qid()
            } else {
                let file = namespace.files.get(&object).ok_or_else(|| {
                    AppFailure::Internal("name points to a missing file record".to_owned())
                })?;
                file_qid(object, file.version)?
            };
            current = object;
            elements.push(WalkElement {
                object: ObjectHandle::new(object),
                qid,
            });
        }
        Ok(FilesystemResult::Walked(WalkResult { elements }))
    }

    async fn open(
        &self,
        object: ObjectHandle,
        flags: OpenFlags,
    ) -> Result<FilesystemResult, AppFailure> {
        let mut namespace = self.namespace.lock().await;
        let file = namespace
            .files
            .get(&object.get())
            .ok_or(AppFailure::Client(LinuxErrno::ENOENT))?;
        let qid = file_qid(object.get(), file.version)?;
        let (readable, writable) = access_mode(flags)?;
        let open = allocate_open(&mut namespace, object.get(), readable, writable)?;
        Ok(FilesystemResult::Opened(OpenResult {
            qid,
            open: OpenHandle::new(open),
            io_unit: 32 * 1024,
        }))
    }

    async fn create(
        &self,
        directory: ObjectHandle,
        name: String,
        flags: OpenFlags,
    ) -> Result<FilesystemResult, AppFailure> {
        if directory.get() != ROOT_OBJECT {
            return Err(AppFailure::Client(LinuxErrno::ENOTDIR));
        }
        let (readable, writable) = access_mode(flags)?;
        let mut namespace = self.namespace.lock().await;
        if namespace.names.contains_key(&name) {
            return Err(AppFailure::Client(LinuxErrno::EEXIST));
        }
        let object = namespace.next_object;
        namespace.next_object = namespace
            .next_object
            .checked_add(1)
            .ok_or(AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        let key = format!("{}/files/{object:032x}", self.prefix);
        self.put_bytes(&key, Vec::new()).await?;
        namespace.names.insert(name, object);
        namespace
            .files
            .insert(object, FileEntry { key, version: 1 });
        let open = allocate_open(&mut namespace, object, readable, writable)?;
        Ok(FilesystemResult::Created(CreateResult {
            object: ObjectHandle::new(object),
            qid: file_qid(object, 1)?,
            open: OpenHandle::new(open),
            io_unit: 32 * 1024,
        }))
    }

    async fn read(
        &self,
        open: OpenHandle,
        offset: u64,
        count: u32,
    ) -> Result<FilesystemResult, AppFailure> {
        let key = {
            let namespace = self.namespace.lock().await;
            let open = namespace
                .opens
                .get(&open.get())
                .ok_or(AppFailure::Client(LinuxErrno::EBADF))?;
            if !open.readable {
                return Err(AppFailure::Client(LinuxErrno::EBADF));
            }
            namespace
                .files
                .get(&open.object)
                .ok_or(AppFailure::Client(LinuxErrno::ENOENT))?
                .key
                .clone()
        };
        let bytes = self.get_bytes(&key).await?;
        let start =
            usize::try_from(offset).map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        if start >= bytes.len() {
            return Ok(FilesystemResult::Read(Vec::new()));
        }
        let requested =
            usize::try_from(count).map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        let end = start.saturating_add(requested).min(bytes.len());
        Ok(FilesystemResult::Read(bytes[start..end].to_vec()))
    }

    async fn write(
        &self,
        open: OpenHandle,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<FilesystemResult, AppFailure> {
        let mut namespace = self.namespace.lock().await;
        let open = *namespace
            .opens
            .get(&open.get())
            .ok_or(AppFailure::Client(LinuxErrno::EBADF))?;
        if !open.writable {
            return Err(AppFailure::Client(LinuxErrno::EBADF));
        }
        let key = namespace
            .files
            .get(&open.object)
            .ok_or(AppFailure::Client(LinuxErrno::ENOENT))?
            .key
            .clone();
        let start =
            usize::try_from(offset).map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        let end = start
            .checked_add(data.len())
            .ok_or(AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        if end > MAX_FILE_BYTES {
            return Err(AppFailure::Client(LinuxErrno::ENOMEM));
        }
        let mut bytes = self.get_bytes(&key).await?;
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(&data);
        self.put_bytes(&key, bytes).await?;
        let file = namespace
            .files
            .get_mut(&open.object)
            .ok_or(AppFailure::Client(LinuxErrno::ENOENT))?;
        file.version = file
            .version
            .checked_add(1)
            .ok_or(AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        let count =
            u32::try_from(data.len()).map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?;
        Ok(FilesystemResult::Written(count))
    }

    async fn get_bytes(&self, key: &str) -> Result<Vec<u8>, AppFailure> {
        let output = self
            .client
            .get_object()
            .bucket(self.bucket.as_ref())
            .key(key)
            .send()
            .await
            .map_err(|error| AppFailure::Internal(format!("S3 GetObject failed: {error}")))?;
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|error| AppFailure::Internal(format!("S3 body read failed: {error}")))?
            .into_bytes();
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AppFailure::Client(LinuxErrno::ENOMEM));
        }
        Ok(bytes.to_vec())
    }

    async fn put_bytes(&self, key: &str, bytes: Vec<u8>) -> Result<(), AppFailure> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AppFailure::Client(LinuxErrno::ENOMEM));
        }
        self.client
            .put_object()
            .bucket(self.bucket.as_ref())
            .key(key)
            .content_length(
                i64::try_from(bytes.len())
                    .map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?,
            )
            .checksum_algorithm(ChecksumAlgorithm::Crc32C)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(|error| AppFailure::Internal(format!("S3 PutObject failed: {error}")))?;
        Ok(())
    }
}

#[derive(Debug)]
enum AppFailure {
    Client(LinuxErrno),
    Internal(String),
}

fn allocate_open(
    namespace: &mut Namespace,
    object: u128,
    readable: bool,
    writable: bool,
) -> Result<u128, AppFailure> {
    let open = namespace.next_open;
    namespace.next_open = namespace
        .next_open
        .checked_add(1)
        .ok_or(AppFailure::Client(LinuxErrno::EOVERFLOW))?;
    namespace.opens.insert(
        open,
        OpenEntry {
            object,
            readable,
            writable,
        },
    );
    Ok(open)
}

fn access_mode(flags: OpenFlags) -> Result<(bool, bool), AppFailure> {
    let allowed =
        OpenFlags::ACCESS_MASK.bits() | OpenFlags::LARGEFILE.bits() | OpenFlags::CLOEXEC.bits();
    if flags.bits() & !allowed != 0 {
        return Err(AppFailure::Client(LinuxErrno::EOPNOTSUPP));
    }
    match flags.bits() & OpenFlags::ACCESS_MASK.bits() {
        0 => Ok((true, false)),
        1 => Ok((false, true)),
        2 => Ok((true, true)),
        _ => Err(AppFailure::Client(LinuxErrno::EINVAL)),
    }
}

fn root_qid() -> Qid {
    Qid::new(QidType::DIRECTORY, 0, 1)
}

fn file_qid(object: u128, version: u32) -> Result<Qid, AppFailure> {
    let path = u64::try_from(object).map_err(|_| AppFailure::Client(LinuxErrno::EOVERFLOW))?;
    Ok(Qid::new(QidType::FILE, version, path))
}

fn application_capabilities() -> CapabilitySet {
    CapabilitySet::from_capabilities([
        Capability::Walk,
        Capability::Open,
        Capability::Create,
        Capability::Read,
        Capability::Write,
        Capability::StableIdentity,
        Capability::AtomicAuthorization,
        Capability::AtomicNamespace,
        Capability::PositionedIo,
        Capability::OpenUnlinked,
    ])
}

/// Creates an AWS SDK client configured for a path-style SeaweedFS S3 endpoint.
pub fn seaweed_client(endpoint: &str) -> Client {
    Client::from_conf(
        Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "w9pt-test-access-key",
                "w9pt-test-secret-key",
                None,
                None,
                "w9pt-tcp-test",
            ))
            .region(Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    )
}

/// Creates the configured bucket when it does not already exist.
pub async fn ensure_bucket(client: &Client, bucket: &str) -> Result<(), BoxError> {
    if client.head_bucket().bucket(bucket).send().await.is_ok() {
        return Ok(());
    }
    if let Err(create_error) = client.create_bucket().bucket(bucket).send().await
        && client.head_bucket().bucket(bucket).send().await.is_err()
    {
        return Err(format!("cannot create SeaweedFS test bucket: {create_error}").into());
    }
    Ok(())
}

/// Serves one accepted TCP connection until the peer closes or the session terminates.
pub async fn serve_connection(
    mut stream: TcpStream,
    filesystem: AppFilesystem,
    session_id: SessionId,
) -> Result<(), BoxError> {
    let mut session = Session::new(SessionConfig::default(), SessionContext::new(session_id))?;
    let mut input = vec![0_u8; 64 * 1024];
    loop {
        let read = stream.read(&mut input).await?;
        if read == 0 {
            session.transport_closed();
            return Ok(());
        }
        session.receive_bytes(&input[..read])?;
        if !drive_effects(&mut session, &mut stream, &filesystem).await? {
            return Ok(());
        }
    }
}

/// Accepts and serves exactly one TCP connection.
pub async fn serve_one(
    listener: TcpListener,
    filesystem: AppFilesystem,
    session_id: SessionId,
) -> Result<(), BoxError> {
    let (stream, _) = listener.accept().await?;
    serve_connection(stream, filesystem, session_id).await
}

/// Runs the demonstration server until its process is stopped.
pub async fn serve_forever(
    listener: TcpListener,
    filesystem: AppFilesystem,
) -> Result<(), BoxError> {
    let mut next_session = 1_u64;
    loop {
        let (stream, _) = listener.accept().await?;
        let session_id = SessionId::new(next_session);
        next_session = next_session
            .checked_add(1)
            .ok_or_else(|| io::Error::other("session identifier exhausted"))?;
        let filesystem = filesystem.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, filesystem, session_id).await {
                eprintln!("9P test connection failed: {error}");
            }
        });
    }
}

#[derive(Clone, Debug)]
struct WebsocketState {
    filesystem: AppFilesystem,
    next_session: Arc<Mutex<u64>>,
}

impl WebsocketState {
    fn new(filesystem: AppFilesystem) -> Self {
        Self {
            filesystem,
            next_session: Arc::new(Mutex::new(1)),
        }
    }

    async fn reserve_session(&self) -> Result<SessionId, io::Error> {
        let mut next = self.next_session.lock().await;
        let session = *next;
        *next = next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("session identifier exhausted"))?;
        Ok(SessionId::new(session))
    }
}

/// Builds the standalone HTTP router that serves 9P over binary WebSocket messages.
///
/// `GET /healthz` returns `204 No Content`. `/9p` requires the `9p` WebSocket
/// subprotocol. Each accepted binary message is one complete 9P frame, and each
/// response frame is sent as one binary message.
pub fn websocket_router(filesystem: AppFilesystem) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/9p", any(upgrade_websocket))
        .with_state(WebsocketState::new(filesystem))
}

async fn upgrade_websocket(
    websocket: WebSocketUpgrade,
    State(state): State<WebsocketState>,
) -> Response {
    let websocket = websocket
        .max_message_size(WEBSOCKET_9P_MAX_MESSAGE_BYTES)
        .max_frame_size(WEBSOCKET_9P_MAX_MESSAGE_BYTES)
        .protocols([WEBSOCKET_9P_SUBPROTOCOL]);
    if websocket.selected_protocol().is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "Sec-WebSocket-Protocol must include 9p",
        )
            .into_response();
    }
    let session_id = match state.reserve_session().await {
        Ok(session_id) => session_id,
        Err(error) => {
            eprintln!("WebSocket 9P session allocation failed: {error}");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    websocket
        .on_failed_upgrade(|error| eprintln!("WebSocket 9P upgrade failed: {error}"))
        .on_upgrade(move |socket| async move {
            if let Err(error) = serve_websocket(socket, state.filesystem, session_id).await {
                eprintln!("WebSocket 9P connection failed: {error}");
            }
        })
}

async fn serve_websocket(
    mut socket: WebSocket,
    filesystem: AppFilesystem,
    session_id: SessionId,
) -> Result<(), BoxError> {
    let mut session = Session::new(SessionConfig::default(), SessionContext::new(session_id))?;
    while let Some(message) = socket.recv().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                session.transport_closed();
                drain_cleanup_effects(&mut session, &filesystem).await?;
                return Err(error.into());
            }
        };
        match message {
            Message::Binary(frame) => {
                if let Err(error) = session.receive_frame(frame.to_vec()) {
                    eprintln!("invalid 9P WebSocket frame: {error}");
                    close_websocket(&mut socket, close_code::PROTOCOL, "invalid 9P frame").await;
                    session.transport_closed();
                    drain_cleanup_effects(&mut session, &filesystem).await?;
                    return Ok(());
                }
                if !drive_websocket_effects(&mut session, &mut socket, &filesystem).await? {
                    return Ok(());
                }
            }
            Message::Text(_) => {
                close_websocket(
                    &mut socket,
                    close_code::UNSUPPORTED,
                    "9P requires binary messages",
                )
                .await;
                session.transport_closed();
                drain_cleanup_effects(&mut session, &filesystem).await?;
                return Ok(());
            }
            Message::Close(_) => {
                session.transport_closed();
                drain_cleanup_effects(&mut session, &filesystem).await?;
                return Ok(());
            }
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    session.transport_closed();
    drain_cleanup_effects(&mut session, &filesystem).await?;
    Ok(())
}

async fn drive_websocket_effects(
    session: &mut Session,
    socket: &mut WebSocket,
    filesystem: &AppFilesystem,
) -> Result<bool, BoxError> {
    while let Some(effect) = session.poll_effect() {
        match effect {
            Effect::SendFrame { bytes } => socket.send(Message::Binary(bytes.into())).await?,
            Effect::Filesystem {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_filesystem(request).await;
                session.complete(Completion::Filesystem {
                    operation_id,
                    result,
                })?;
            }
            Effect::Policy {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_policy(request);
                session.complete(Completion::Policy {
                    operation_id,
                    result,
                })?;
            }
            Effect::Cancel { .. } => {}
            Effect::CloseSession { .. } => {
                close_websocket(socket, close_code::PROTOCOL, "9P session closed").await;
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn drain_cleanup_effects(
    session: &mut Session,
    filesystem: &AppFilesystem,
) -> Result<(), BoxError> {
    while let Some(effect) = session.poll_effect() {
        match effect {
            Effect::Filesystem {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_filesystem(request).await;
                session.complete(Completion::Filesystem {
                    operation_id,
                    result,
                })?;
            }
            Effect::Policy {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_policy(request);
                session.complete(Completion::Policy {
                    operation_id,
                    result,
                })?;
            }
            Effect::SendFrame { .. } | Effect::Cancel { .. } | Effect::CloseSession { .. } => {}
        }
    }
    Ok(())
}

async fn close_websocket(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn drive_effects(
    session: &mut Session,
    stream: &mut TcpStream,
    filesystem: &AppFilesystem,
) -> Result<bool, BoxError> {
    while let Some(effect) = session.poll_effect() {
        match effect {
            Effect::SendFrame { bytes } => stream.write_all(&bytes).await?,
            Effect::Filesystem {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_filesystem(request).await;
                session.complete(Completion::Filesystem {
                    operation_id,
                    result,
                })?;
            }
            Effect::Policy {
                operation_id,
                request,
            } => {
                let result = filesystem.handle_policy(request);
                session.complete(Completion::Policy {
                    operation_id,
                    result,
                })?;
            }
            Effect::Cancel { .. } => {}
            Effect::CloseSession { .. } => return Ok(false),
        }
    }
    Ok(true)
}
