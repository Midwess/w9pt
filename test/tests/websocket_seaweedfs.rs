//! End-to-end HTTP/WebSocket 9P forwarding test backed by SeaweedFS's S3 API.

mod support;

use std::{env, error::Error, future::Future, io, net::SocketAddr, pin::Pin, time::Duration};

use aws_sdk_s3::Client;
use futures_util::{SinkExt, StreamExt};
use support::{NinePTransport, ROOT_FID, attach, exercise_file_lifecycle, negotiate};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        self, Message,
        client::IntoClientRequest,
        http::{HeaderValue, StatusCode, header::SEC_WEBSOCKET_PROTOCOL},
        protocol::frame::coding::CloseCode,
    },
};
use w9pt_integration_test::{
    AppFilesystem, WEBSOCKET_9P_SUBPROTOCOL, ensure_bucket, seaweed_client, websocket_router,
};

type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TEST_FILE: &str = "websocket.txt";
const TEST_DATA: &[u8] = b"hello from 9P over WebSocket through SeaweedFS";

struct WebsocketNinePTransport(ClientSocket);

impl NinePTransport for WebsocketNinePTransport {
    fn exchange(
        &mut self,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async move {
            self.0
                .send(Message::Binary(request.into()))
                .await
                .map_err(io::Error::other)?;
            while let Some(message) = self.0.next().await {
                match message.map_err(io::Error::other)? {
                    Message::Binary(frame) => return Ok(frame.to_vec()),
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    Message::Text(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "server returned a text message for a 9P response",
                        ));
                    }
                    Message::Close(frame) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            format!("server closed before returning a 9P response: {frame:?}"),
                        ));
                    }
                }
            }
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WebSocket ended before returning a 9P response",
            ))
        })
    }
}

struct TestHttpServer {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl TestHttpServer {
    async fn start(filesystem: AppFilesystem) -> io::Result<(Self, SocketAddr)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, websocket_router(filesystem))
                .with_graceful_shutdown(async move {
                    let _ = shutdown_receiver.await;
                })
                .await
        });
        Ok((
            Self {
                shutdown: Some(shutdown),
                task: Some(task),
            },
            address,
        ))
    }

    async fn stop(mut self) -> Result<(), BoxError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_9p_effects_are_completed_by_seaweedfs_s3_application() -> Result<(), BoxError> {
    if env::var("W9PT_WEBSOCKET_SEAWEED_TEST_REQUIRED").as_deref() != Ok("1") {
        eprintln!(
            "skipping WebSocket SeaweedFS integration; set W9PT_WEBSOCKET_SEAWEED_TEST_REQUIRED=1 to require it"
        );
        return Ok(());
    }

    timeout(Duration::from_secs(60), required_websocket_profile())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "WebSocket profile timed out"))??;
    Ok(())
}

async fn required_websocket_profile() -> Result<(), BoxError> {
    let endpoint = required("W9PT_TEST_S3_ENDPOINT")?;
    let bucket = required("W9PT_TEST_S3_BUCKET")?;
    let prefix = required("W9PT_TEST_WEBSOCKET_S3_PREFIX")?;
    let client = seaweed_client(&endpoint);
    ensure_bucket(&client, &bucket).await?;
    let filesystem = AppFilesystem::new(client.clone(), bucket.clone(), prefix.clone());
    let (server, address) = TestHttpServer::start(filesystem.clone()).await?;

    assert_eq!(http_status(address, "/healthz").await?, 204);
    assert_missing_subprotocol_is_rejected(address).await?;

    let (mut transport, response) = connect_9p(address).await?;
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response.headers().get(SEC_WEBSOCKET_PROTOCOL),
        Some(&HeaderValue::from_static(WEBSOCKET_9P_SUBPROTOCOL))
    );
    let readback = exercise_file_lifecycle(&mut transport, TEST_FILE, TEST_DATA).await?;
    assert_eq!(readback, TEST_DATA);
    transport.0.close(None).await?;

    let key = filesystem
        .file_key(TEST_FILE)
        .await
        .ok_or("test namespace lost the WebSocket-created file")?;
    assert!(key.starts_with(&format!("{prefix}/files/")));
    assert_eq!(s3_bytes(&client, &bucket, &key).await?, TEST_DATA);

    let (mut text_client, _) = connect_9p(address).await?;
    negotiate(&mut text_client).await?;
    attach(&mut text_client, 1, ROOT_FID).await?;
    text_client.0.send(Message::Text("not 9P".into())).await?;
    assert_close_code(&mut text_client.0, CloseCode::Unsupported).await?;

    let (mut malformed_client, _) = connect_9p(address).await?;
    malformed_client
        .0
        .send(Message::Binary(vec![4_u8, 0, 0, 0].into()))
        .await?;
    assert_close_code(&mut malformed_client.0, CloseCode::Protocol).await?;

    client
        .delete_object()
        .bucket(&bucket)
        .key(key)
        .send()
        .await?;
    server.stop().await?;
    Ok(())
}

async fn connect_9p(
    address: SocketAddr,
) -> Result<
    (
        WebsocketNinePTransport,
        tungstenite::handshake::client::Response,
    ),
    BoxError,
> {
    let mut request = format!("ws://{address}/9p").into_client_request()?;
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(WEBSOCKET_9P_SUBPROTOCOL),
    );
    let (socket, response) = connect_async(request).await?;
    Ok((WebsocketNinePTransport(socket), response))
}

async fn assert_missing_subprotocol_is_rejected(address: SocketAddr) -> Result<(), BoxError> {
    match connect_async(format!("ws://{address}/9p")).await {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            Ok(())
        }
        Err(error) => Err(format!("unexpected missing-subprotocol error: {error}").into()),
        Ok(_) => Err("WebSocket upgrade without the 9p subprotocol succeeded".into()),
    }
}

async fn assert_close_code(socket: &mut ClientSocket, expected: CloseCode) -> Result<(), BoxError> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Close(Some(frame)) if frame.code == expected => return Ok(()),
            Message::Close(frame) => {
                return Err(format!("unexpected WebSocket close frame: {frame:?}").into());
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Text(_) | Message::Binary(_) => {
                return Err("received data while waiting for WebSocket close".into());
            }
        }
    }
    Err("WebSocket ended without the expected close frame".into())
}

async fn http_status(address: SocketAddr, path: &str) -> io::Result<u16> {
    let mut stream = TcpStream::connect(address).await?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    let mut response = Vec::with_capacity(512);
    let mut chunk = [0_u8; 256];
    while response.len() < 4_096 && !response.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
    }
    let line_end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status line"))?;
    let status_line = std::str::from_utf8(&response[..line_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let status = status_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status code"))?;
    status
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn required(name: &str) -> Result<String, BoxError> {
    env::var(name)
        .map_err(|_| format!("{name} is required for WebSocket SeaweedFS integration").into())
}

async fn s3_bytes(client: &Client, bucket: &str, key: &str) -> Result<Vec<u8>, BoxError> {
    Ok(client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await?
        .body
        .collect()
        .await?
        .into_bytes()
        .to_vec())
}
