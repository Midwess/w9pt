//! End-to-end TCP 9P forwarding test backed by SeaweedFS's S3 API.

use std::{env, error::Error, io};

use aws_sdk_s3::Client;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use w9pt::{
    SessionId, Tag,
    protocol::{Fid, MessageType, OpenFlags, VERSION_9P2000_L},
};
use w9pt_tcp_seaweedfs_test::{AppFilesystem, ensure_bucket, seaweed_client, serve_one};

const ROOT_FID: u32 = 1;
const REATTACH_FID: u32 = 2;
const FILE_FID: u32 = 3;
const TEST_FILE: &str = "hello.txt";
const TEST_DATA: &[u8] = b"hello from 9P through SeaweedFS";

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
    let mut connection = TcpStream::connect(address).await?;

    negotiate(&mut connection).await?;
    attach(&mut connection, 1, ROOT_FID).await?;
    create(&mut connection, 2, ROOT_FID, TEST_FILE).await?;
    write(&mut connection, 3, ROOT_FID, TEST_DATA).await?;
    clunk(&mut connection, 4, ROOT_FID).await?;

    attach(&mut connection, 5, REATTACH_FID).await?;
    walk(&mut connection, 6, REATTACH_FID, FILE_FID, TEST_FILE).await?;
    open(&mut connection, 7, FILE_FID).await?;
    let readback = read(&mut connection, 8, FILE_FID, 0, 4_096).await?;
    assert_eq!(readback, TEST_DATA);
    clunk(&mut connection, 9, FILE_FID).await?;
    clunk(&mut connection, 10, REATTACH_FID).await?;

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

    connection.shutdown().await?;
    drop(connection);
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

async fn negotiate(stream: &mut TcpStream) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&65_536_u32.to_le_bytes());
    push_string(&mut body, VERSION_9P2000_L)?;
    let response = exchange(
        stream,
        request(MessageType::Tversion, Tag::NOTAG.get(), body)?,
    )
    .await?;
    expect_response(&response, MessageType::Rversion, Tag::NOTAG.get())?;
    Ok(())
}

async fn attach(stream: &mut TcpStream, tag: u16, fid: u32) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&Fid::NOFID.get().to_le_bytes());
    push_string(&mut body, "test-user")?;
    push_string(&mut body, "seaweedfs-test")?;
    body.extend_from_slice(&1_000_u32.to_le_bytes());
    let response = exchange(stream, request(MessageType::Tattach, tag, body)?).await?;
    expect_response(&response, MessageType::Rattach, tag)?;
    require_len(&response, 7 + 13)?;
    Ok(())
}

async fn create(stream: &mut TcpStream, tag: u16, fid: u32, name: &str) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    push_string(&mut body, name)?;
    body.extend_from_slice(&OpenFlags::RDWR.bits().to_le_bytes());
    body.extend_from_slice(&0o644_u32.to_le_bytes());
    body.extend_from_slice(&1_000_u32.to_le_bytes());
    let response = exchange(stream, request(MessageType::Tlcreate, tag, body)?).await?;
    expect_response(&response, MessageType::Rlcreate, tag)?;
    require_len(&response, 7 + 13 + 4)?;
    Ok(())
}

async fn write(stream: &mut TcpStream, tag: u16, fid: u32, data: &[u8]) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&0_u64.to_le_bytes());
    body.extend_from_slice(&u32_len(data.len())?.to_le_bytes());
    body.extend_from_slice(data);
    let response = exchange(stream, request(MessageType::Twrite, tag, body)?).await?;
    expect_response(&response, MessageType::Rwrite, tag)?;
    require_len(&response, 11)?;
    assert_eq!(
        u32::from_le_bytes(response[7..11].try_into().unwrap()),
        u32_len(data.len())?
    );
    Ok(())
}

async fn walk(
    stream: &mut TcpStream,
    tag: u16,
    fid: u32,
    new_fid: u32,
    name: &str,
) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&new_fid.to_le_bytes());
    body.extend_from_slice(&1_u16.to_le_bytes());
    push_string(&mut body, name)?;
    let response = exchange(stream, request(MessageType::Twalk, tag, body)?).await?;
    expect_response(&response, MessageType::Rwalk, tag)?;
    require_len(&response, 7 + 2 + 13)?;
    assert_eq!(u16::from_le_bytes(response[7..9].try_into().unwrap()), 1);
    Ok(())
}

async fn open(stream: &mut TcpStream, tag: u16, fid: u32) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&OpenFlags::RDONLY.bits().to_le_bytes());
    let response = exchange(stream, request(MessageType::Tlopen, tag, body)?).await?;
    expect_response(&response, MessageType::Rlopen, tag)?;
    require_len(&response, 7 + 13 + 4)?;
    Ok(())
}

async fn read(
    stream: &mut TcpStream,
    tag: u16,
    fid: u32,
    offset: u64,
    count: u32,
) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&offset.to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    let response = exchange(stream, request(MessageType::Tread, tag, body)?).await?;
    expect_response(&response, MessageType::Rread, tag)?;
    require_len(&response, 11)?;
    let length = u32::from_le_bytes(response[7..11].try_into().unwrap()) as usize;
    require_len(&response, 11 + length)?;
    Ok(response[11..].to_vec())
}

async fn clunk(stream: &mut TcpStream, tag: u16, fid: u32) -> io::Result<()> {
    let response = exchange(
        stream,
        request(MessageType::Tclunk, tag, fid.to_le_bytes().to_vec())?,
    )
    .await?;
    expect_response(&response, MessageType::Rclunk, tag)?;
    require_len(&response, 7)
}

fn request(message: MessageType, tag: u16, body: Vec<u8>) -> io::Result<Vec<u8>> {
    let length = 7_usize
        .checked_add(body.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "request length overflow"))?;
    let mut frame = Vec::with_capacity(length);
    frame.extend_from_slice(&u32_len(length)?.to_le_bytes());
    frame.push(message.to_u8());
    frame.extend_from_slice(&tag.to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

async fn exchange(stream: &mut TcpStream, request: Vec<u8>) -> io::Result<Vec<u8>> {
    stream.write_all(&request).await?;
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await?;
    let length = u32::from_le_bytes(header) as usize;
    if !(7..=65_536).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid response frame length {length}"),
        ));
    }
    let mut frame = vec![0_u8; length];
    frame[..4].copy_from_slice(&header);
    stream.read_exact(&mut frame[4..]).await?;
    Ok(frame)
}

fn expect_response(frame: &[u8], message: MessageType, tag: u16) -> io::Result<()> {
    require_len(frame, 7)?;
    if frame[4] == MessageType::Rlerror.to_u8() {
        require_len(frame, 11)?;
        let errno = u32::from_le_bytes(frame[7..11].try_into().unwrap());
        return Err(io::Error::other(format!(
            "9P server returned Linux errno {errno}"
        )));
    }
    if frame[4] != message.to_u8() || u16::from_le_bytes(frame[5..7].try_into().unwrap()) != tag {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected 9P response type or tag",
        ));
    }
    Ok(())
}

fn push_string(buffer: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length = u16::try_from(value.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "9P string is too long"))?;
    buffer.extend_from_slice(&length.to_le_bytes());
    buffer.extend_from_slice(value.as_bytes());
    Ok(())
}

fn require_len(bytes: &[u8], minimum: usize) -> io::Result<()> {
    if bytes.len() < minimum {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short 9P response",
        ))
    } else {
        Ok(())
    }
}

fn u32_len(length: usize) -> io::Result<u32> {
    u32::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length exceeds u32"))
}
