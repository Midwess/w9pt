//! End-to-end TCP 9P forwarding test backed by SeaweedFS's S3 API.

mod support;

use std::{env, error::Error, future::Future, io, pin::Pin};

use aws_sdk_s3::Client;
use support::{NinePTransport, exercise_file_lifecycle};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use w9pt::SessionId;
use w9pt_integration_test::{AppFilesystem, ensure_bucket, seaweed_client, serve_one};

const TEST_FILE: &str = "hello.txt";
const TEST_DATA: &[u8] = b"hello from 9P through SeaweedFS";

struct TcpNinePTransport(TcpStream);

impl NinePTransport for TcpNinePTransport {
    fn exchange(
        &mut self,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + '_>> {
        Box::pin(async move {
            self.0.write_all(&request).await?;
            let mut header = [0_u8; 4];
            self.0.read_exact(&mut header).await?;
            let length = u32::from_le_bytes(header) as usize;
            if !(7..=65_536).contains(&length) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid response frame length {length}"),
                ));
            }
            let mut frame = vec![0_u8; length];
            frame[..4].copy_from_slice(&header);
            self.0.read_exact(&mut frame[4..]).await?;
            Ok(frame)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_9p_effects_are_completed_by_seaweedfs_s3_application()
-> Result<(), Box<dyn Error + Send + Sync>> {
    if env::var("W9PT_TCP_SEAWEED_TEST_REQUIRED").as_deref() != Ok("1") {
        eprintln!(
            "skipping TCP SeaweedFS integration; set W9PT_TCP_SEAWEED_TEST_REQUIRED=1 to require it"
        );
        return Ok(());
    }

    let endpoint = required("W9PT_TEST_S3_ENDPOINT")?;
    let bucket = required("W9PT_TEST_S3_BUCKET")?;
    let prefix = required("W9PT_TEST_S3_PREFIX")?;
    let client = seaweed_client(&endpoint);
    ensure_bucket(&client, &bucket).await?;
    let filesystem = AppFilesystem::new(client.clone(), bucket.clone(), prefix.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_one(listener, filesystem.clone(), SessionId::new(1)));
    let mut transport = TcpNinePTransport(TcpStream::connect(address).await?);

    let readback = exercise_file_lifecycle(&mut transport, TEST_FILE, TEST_DATA).await?;
    assert_eq!(readback, TEST_DATA);

    let key = filesystem
        .file_key(TEST_FILE)
        .await
        .ok_or("test namespace lost the created file")?;
    assert!(key.starts_with(&format!("{prefix}/files/")));
    assert_eq!(s3_bytes(&client, &bucket, &key).await?, TEST_DATA);
    client
        .delete_object()
        .bucket(&bucket)
        .key(key)
        .send()
        .await?;

    transport.0.shutdown().await?;
    drop(transport);
    server.await??;
    Ok(())
}

fn required(name: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    env::var(name).map_err(|_| format!("{name} is required for TCP SeaweedFS integration").into())
}

async fn s3_bytes(
    client: &Client,
    bucket: &str,
    key: &str,
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
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
