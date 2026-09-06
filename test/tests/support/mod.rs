//! Transport-neutral 9P client helpers shared by integration profiles.

#![allow(dead_code)]

pub mod content_target;

use std::{future::Future, io, pin::Pin};

use w9pt::{
    Tag,
    protocol::{Fid, MessageType, OpenFlags, VERSION_9P2000_L},
};

pub const ROOT_FID: u32 = 1;
pub const REATTACH_FID: u32 = 2;
pub const FILE_FID: u32 = 3;

pub trait NinePTransport {
    fn exchange(
        &mut self,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send + '_>>;
}

pub async fn exercise_file_lifecycle(
    transport: &mut impl NinePTransport,
    file_name: &str,
    test_data: &[u8],
) -> io::Result<Vec<u8>> {
    negotiate(transport).await?;
    attach(transport, 1, ROOT_FID).await?;
    create(transport, 2, ROOT_FID, file_name).await?;
    write(transport, 3, ROOT_FID, test_data).await?;
    clunk(transport, 4, ROOT_FID).await?;

    attach(transport, 5, REATTACH_FID).await?;
    walk(transport, 6, REATTACH_FID, FILE_FID, file_name).await?;
    open(transport, 7, FILE_FID).await?;
    let readback = read(transport, 8, FILE_FID, 0, 4_096).await?;
    clunk(transport, 9, FILE_FID).await?;
    clunk(transport, 10, REATTACH_FID).await?;
    Ok(readback)
}

pub async fn negotiate(transport: &mut impl NinePTransport) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&65_536_u32.to_le_bytes());
    push_string(&mut body, VERSION_9P2000_L)?;
    let response = transport
        .exchange(request(MessageType::Tversion, Tag::NOTAG.get(), body)?)
        .await?;
    expect_response(&response, MessageType::Rversion, Tag::NOTAG.get())?;
    Ok(())
}

pub async fn attach(transport: &mut impl NinePTransport, tag: u16, fid: u32) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&Fid::NOFID.get().to_le_bytes());
    push_string(&mut body, "test-user")?;
    push_string(&mut body, "seaweedfs-test")?;
    body.extend_from_slice(&1_000_u32.to_le_bytes());
    let response = transport
        .exchange(request(MessageType::Tattach, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rattach, tag)?;
    require_len(&response, 7 + 13)?;
    Ok(())
}

async fn create(
    transport: &mut impl NinePTransport,
    tag: u16,
    fid: u32,
    name: &str,
) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    push_string(&mut body, name)?;
    body.extend_from_slice(&OpenFlags::RDWR.bits().to_le_bytes());
    body.extend_from_slice(&0o644_u32.to_le_bytes());
    body.extend_from_slice(&1_000_u32.to_le_bytes());
    let response = transport
        .exchange(request(MessageType::Tlcreate, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rlcreate, tag)?;
    require_len(&response, 7 + 13 + 4)?;
    Ok(())
}

async fn write(
    transport: &mut impl NinePTransport,
    tag: u16,
    fid: u32,
    data: &[u8],
) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&0_u64.to_le_bytes());
    body.extend_from_slice(&u32_len(data.len())?.to_le_bytes());
    body.extend_from_slice(data);
    let response = transport
        .exchange(request(MessageType::Twrite, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rwrite, tag)?;
    require_len(&response, 11)?;
    assert_eq!(
        u32::from_le_bytes(response[7..11].try_into().unwrap()),
        u32_len(data.len())?
    );
    Ok(())
}

async fn walk(
    transport: &mut impl NinePTransport,
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
    let response = transport
        .exchange(request(MessageType::Twalk, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rwalk, tag)?;
    require_len(&response, 7 + 2 + 13)?;
    assert_eq!(u16::from_le_bytes(response[7..9].try_into().unwrap()), 1);
    Ok(())
}

async fn open(transport: &mut impl NinePTransport, tag: u16, fid: u32) -> io::Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&OpenFlags::RDONLY.bits().to_le_bytes());
    let response = transport
        .exchange(request(MessageType::Tlopen, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rlopen, tag)?;
    require_len(&response, 7 + 13 + 4)?;
    Ok(())
}

async fn read(
    transport: &mut impl NinePTransport,
    tag: u16,
    fid: u32,
    offset: u64,
    count: u32,
) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&fid.to_le_bytes());
    body.extend_from_slice(&offset.to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    let response = transport
        .exchange(request(MessageType::Tread, tag, body)?)
        .await?;
    expect_response(&response, MessageType::Rread, tag)?;
    require_len(&response, 11)?;
    let length = u32::from_le_bytes(response[7..11].try_into().unwrap()) as usize;
    require_len(&response, 11 + length)?;
    if response.len() != 11 + length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "9P read response has trailing bytes",
        ));
    }
    Ok(response[11..].to_vec())
}

async fn clunk(transport: &mut impl NinePTransport, tag: u16, fid: u32) -> io::Result<()> {
    let response = transport
        .exchange(request(
            MessageType::Tclunk,
            tag,
            fid.to_le_bytes().to_vec(),
        )?)
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

fn expect_response(frame: &[u8], message: MessageType, tag: u16) -> io::Result<()> {
    require_len(frame, 7)?;
    let declared = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    if declared != frame.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "9P response length does not match message boundary",
        ));
    }
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
