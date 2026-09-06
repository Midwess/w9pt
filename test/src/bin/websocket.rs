//! Standalone HTTP/WebSocket demonstration server for the SeaweedFS integration fixture.

use std::{env, net::SocketAddr};

use tokio::net::TcpListener;
use w9pt_integration_test::{
    AppFilesystem, BoxError, ensure_bucket, seaweed_client, websocket_router,
};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), BoxError> {
    let listen = env::var("W9PT_TEST_WEBSOCKET_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse::<SocketAddr>()?;
    let endpoint =
        env::var("W9PT_TEST_S3_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:18333".to_owned());
    let bucket = env::var("W9PT_TEST_S3_BUCKET").unwrap_or_else(|_| "w9pt-test-bucket".to_owned());
    let prefix = env::var("W9PT_TEST_S3_PREFIX")
        .unwrap_or_else(|_| "websocket-server/w9pt-s3-test-development".to_owned());

    let client = seaweed_client(&endpoint);
    ensure_bucket(&client, &bucket).await?;
    let filesystem = AppFilesystem::new(client, bucket, prefix);
    let listener = TcpListener::bind(listen).await?;
    eprintln!(
        "w9pt WebSocket test server listening on http://{} (9P endpoint /9p)",
        listener.local_addr()?
    );
    axum::serve(listener, websocket_router(filesystem)).await?;
    Ok(())
}
